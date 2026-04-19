//! 3D Path Planning — RRT* algorithm (D03).
//!
//! Rapidly-exploring Random Tree with rewiring (RRT*) for UAV 3D path planning.
//! Finds collision-free paths in a 3D voxel grid from start to goal.
//!
//! ## Coordinate frame
//! All coordinates are in mm (NED: X=North, Y=East, Z=-Altitude).
//!
//! ## Limitations (embedded constraints)
//! - Max nodes: `PATH3D_MAX_NODES` (static allocation, no heap).
//! - Voxel grid: `PATH3D_GRID_SIZE`³ occupancy grid.
//! - Path output: up to `PATH3D_MAX_PATH` waypoints.
//!
//! ## Usage
//! ```rust
//! path3d_reset();
//! // Mark obstacles in the occupancy grid:
//! path3d_mark_obstacle(x_mm, y_mm, z_mm);
//! // Find a path:
//! let n = path3d_plan(&start, &goal, &mut path_out);
//! ```

use robot_os_sync::SpinLock;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum RRT* tree nodes.
pub const PATH3D_MAX_NODES: usize = 512;
/// Maximum output path waypoints.
pub const PATH3D_MAX_PATH:  usize = 64;
/// Voxel grid resolution per axis.
pub const PATH3D_GRID_SIZE: usize = 32;
/// Physical size of the planning volume per axis (mm).
pub const PATH3D_VOLUME_MM: i32 = 100_000; // 100 m
/// Size of one voxel (mm).
pub const PATH3D_VOXEL_MM:  i32 = PATH3D_VOLUME_MM / PATH3D_GRID_SIZE as i32;
/// RRT* step length (mm) — maximum distance to extend tree per iteration.
pub const PATH3D_STEP_MM:   i32 = 3_000; // 3 m
/// RRT* near-radius for rewiring (mm).
pub const PATH3D_NEAR_MM:   i32 = 8_000; // 8 m
/// RRT* maximum planning iterations.
pub const PATH3D_MAX_ITERS: u32 = 2_000;
/// Goal tolerance (mm) — declare success when within this distance of goal.
pub const PATH3D_GOAL_TOL:  i32 = 2_000; // 2 m

// ── Types ─────────────────────────────────────────────────────────────────────

/// A 3D point in mm (NED).
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Point3D {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl Point3D {
    /// Squared Euclidean distance (mm²).  May overflow for very large distances;
    /// safe for distances < ~46 km.
    pub fn dist_sq(&self, other: &Point3D) -> i64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        let dz = self.z - other.z;
        dx as i64 * dx as i64 + dy as i64 * dy as i64 + dz as i64 * dz as i64
    }

    /// Approximate Euclidean distance (mm) using integer sqrt.
    pub fn dist(&self, other: &Point3D) -> i32 {
        isqrt64(self.dist_sq(other)) as i32
    }
}

/// One RRT* tree node.
#[derive(Clone, Copy)]
struct RrtNode {
    pos:    Point3D,
    parent: u16,         // index of parent node; u16::MAX = root
    cost:   i32,         // cost from root (mm)
}

const RRTNODE_EMPTY: RrtNode = RrtNode {
    pos: Point3D { x: 0, y: 0, z: 0 },
    parent: u16::MAX,
    cost: 0,
};

// ── Occupancy grid ────────────────────────────────────────────────────────────

/// 3D occupancy grid: 1 bit per voxel, packed into u32 words.
/// Total: 32³ = 32768 voxels = 1024 u32 words = 4 KiB.
const GRID_WORDS: usize = PATH3D_GRID_SIZE * PATH3D_GRID_SIZE * PATH3D_GRID_SIZE / 32;

struct Path3dState {
    grid:      [u32; GRID_WORDS],
    nodes:     [RrtNode; PATH3D_MAX_NODES],
    node_count: u16,
    /// Pseudo-random seed for tree sampling.
    rng:       u32,
}

impl Path3dState {
    const fn new() -> Self {
        Path3dState {
            grid: [0; GRID_WORDS],
            nodes: [RRTNODE_EMPTY; PATH3D_MAX_NODES],
            node_count: 0,
            rng: 0xDEAD_BEEF,
        }
    }
}

static PATH3D: SpinLock<Path3dState> = SpinLock::new(Path3dState::new());

// ── Utilities ─────────────────────────────────────────────────────────────────

/// Integer square root (Newton's method, no float).
fn isqrt64(n: i64) -> i64 {
    if n <= 0 { return 0; }
    let mut x = n;
    let mut x1 = (x + 1) / 2;
    while x1 < x {
        x = x1;
        x1 = (x + n / x) / 2;
    }
    x
}

/// Xorshift32 pseudo-random number generator.
fn rng_next(seed: &mut u32) -> u32 {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *seed = x;
    x
}

/// Map a world coordinate to a voxel index (clamp to grid bounds).
fn world_to_voxel(v: i32) -> usize {
    let idx = (v + PATH3D_VOLUME_MM / 2) / PATH3D_VOXEL_MM;
    idx.clamp(0, PATH3D_GRID_SIZE as i32 - 1) as usize
}

fn voxel_index(x: usize, y: usize, z: usize) -> (usize, u32) {
    let flat = x * PATH3D_GRID_SIZE * PATH3D_GRID_SIZE + y * PATH3D_GRID_SIZE + z;
    (flat / 32, 1u32 << (flat % 32))
}

fn is_occupied(state: &Path3dState, pt: &Point3D) -> bool {
    let xi = world_to_voxel(pt.x);
    let yi = world_to_voxel(pt.y);
    let zi = world_to_voxel(pt.z);
    let (word, bit) = voxel_index(xi, yi, zi);
    state.grid[word] & bit != 0
}

/// Steer from `a` toward `b` by at most `step_mm`.
fn steer(a: &Point3D, b: &Point3D, step_mm: i32) -> Point3D {
    let d = a.dist(b);
    if d == 0 || d <= step_mm { return *b; }
    Point3D {
        x: a.x + (b.x - a.x) * step_mm / d,
        y: a.y + (b.y - a.y) * step_mm / d,
        z: a.z + (b.z - a.z) * step_mm / d,
    }
}

/// Check if the straight line from `a` to `b` is collision-free.
/// Samples the line at voxel-resolution intervals.
fn is_free(state: &Path3dState, a: &Point3D, b: &Point3D) -> bool {
    let d = a.dist(b);
    if d == 0 { return true; }
    let steps = (d / (PATH3D_VOXEL_MM / 2)).max(1);
    for i in 0..=steps {
        let pt = Point3D {
            x: a.x + (b.x - a.x) * i / steps,
            y: a.y + (b.y - a.y) * i / steps,
            z: a.z + (b.z - a.z) * i / steps,
        };
        if is_occupied(state, &pt) { return false; }
    }
    true
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Clear the occupancy grid and RRT tree.
pub fn path3d_reset() {
    let mut s = PATH3D.lock();
    s.grid = [0; GRID_WORDS];
    s.nodes = [RRTNODE_EMPTY; PATH3D_MAX_NODES];
    s.node_count = 0;
}

/// Mark a voxel at world position `(x, y, z)` mm as occupied (obstacle).
pub fn path3d_mark_obstacle(x: i32, y: i32, z: i32) {
    let mut s = PATH3D.lock();
    let xi = world_to_voxel(x);
    let yi = world_to_voxel(y);
    let zi = world_to_voxel(z);
    let (word, bit) = voxel_index(xi, yi, zi);
    s.grid[word] |= bit;
}

/// Plan a path from `start` to `goal` using RRT*.
///
/// Returns the number of waypoints written to `path_out`.
/// Returns 0 if no path was found within `PATH3D_MAX_ITERS`.
pub fn path3d_plan(start: &Point3D, goal: &Point3D, path_out: &mut [Point3D]) -> usize {
    let mut s = PATH3D.lock();
    s.nodes[0] = RrtNode { pos: *start, parent: u16::MAX, cost: 0 };
    s.node_count = 1;

    let mut goal_node: Option<u16> = None;

    for _ in 0..PATH3D_MAX_ITERS {
        if s.node_count as usize >= PATH3D_MAX_NODES { break; }

        // Sample random point (bias toward goal 10% of the time).
        let rand = rng_next(&mut s.rng);
        let q_rand = if rand % 10 == 0 {
            *goal
        } else {
            Point3D {
                x: (rand as i32 % PATH3D_VOLUME_MM) - PATH3D_VOLUME_MM / 2,
                y: ((rng_next(&mut s.rng) as i32) % PATH3D_VOLUME_MM) - PATH3D_VOLUME_MM / 2,
                z: ((rng_next(&mut s.rng) as i32) % PATH3D_VOLUME_MM) - PATH3D_VOLUME_MM / 2,
            }
        };

        // Find nearest node.
        let mut nearest_idx = 0u16;
        let mut nearest_dist = i64::MAX;
        for i in 0..s.node_count as usize {
            let d = s.nodes[i].pos.dist_sq(&q_rand);
            if d < nearest_dist { nearest_dist = d; nearest_idx = i as u16; }
        }

        // Steer toward q_rand.
        let q_new = steer(&s.nodes[nearest_idx as usize].pos, &q_rand, PATH3D_STEP_MM);

        if !is_free(&s, &s.nodes[nearest_idx as usize].pos, &q_new) { continue; }

        // Find near nodes within PATH3D_NEAR_MM.
        let new_cost_base = s.nodes[nearest_idx as usize].cost
            + s.nodes[nearest_idx as usize].pos.dist(&q_new);

        // Choose best parent (minimize cost).
        let mut best_parent = nearest_idx;
        let mut best_cost   = new_cost_base;
        for i in 0..s.node_count as usize {
            if s.nodes[i].pos.dist(&q_new) > PATH3D_NEAR_MM { continue; }
            if !is_free(&s, &s.nodes[i].pos, &q_new) { continue; }
            let c = s.nodes[i].cost + s.nodes[i].pos.dist(&q_new);
            if c < best_cost { best_cost = c; best_parent = i as u16; }
        }

        // Add new node.
        let new_idx = s.node_count;
        s.nodes[new_idx as usize] = RrtNode { pos: q_new, parent: best_parent, cost: best_cost };
        s.node_count += 1;

        // Rewire near nodes through q_new if cheaper.
        for i in 0..new_idx as usize {
            let c = best_cost + s.nodes[i].pos.dist(&q_new);
            if c < s.nodes[i].cost && is_free(&s, &q_new, &s.nodes[i].pos) {
                s.nodes[i].parent = new_idx;
                s.nodes[i].cost   = c;
            }
        }

        // Check if we reached the goal.
        if q_new.dist(goal) <= PATH3D_GOAL_TOL {
            match goal_node {
                None    => goal_node = Some(new_idx),
                Some(g) => {
                    if best_cost < s.nodes[g as usize].cost {
                        goal_node = Some(new_idx);
                    }
                }
            }
        }
    }

    // Extract path by tracing back from goal node.
    let goal_idx = match goal_node { Some(g) => g, None => return 0 };

    let mut path_rev = [Point3D::default(); PATH3D_MAX_PATH];
    let mut path_len = 0usize;
    let mut idx = goal_idx;
    loop {
        if path_len >= PATH3D_MAX_PATH { break; }
        path_rev[path_len] = s.nodes[idx as usize].pos;
        path_len += 1;
        let parent = s.nodes[idx as usize].parent;
        if parent == u16::MAX { break; }
        idx = parent;
    }

    // Reverse into output buffer.
    let out_len = path_len.min(path_out.len());
    for i in 0..out_len {
        path_out[i] = path_rev[path_len - 1 - i];
    }
    out_len
}
