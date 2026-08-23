//! Hand-rolled TOML subset parser — RFC-0005.
//!
//! This parser accepts only the subset that PHANES topology files use:
//!
//! - Section headers: `[class.NAME]`, `[task.NAME]`, `[sched]`.
//! - Comments: `# anything until end of line`.
//! - Scalars: bare integer, `"quoted string"`, `true` / `false`.
//! - Range: `[int, int]` (used only for `priority_range`).
//! - Array-of-inline-tables (multi-line):
//!
//!   ```toml
//!   caps = [
//!       { kind = "channel-pub", target = "/x", perm = "w" },
//!       { kind = "channel-sub", target = "/y", perm = "r" },
//!   ]
//!   ```
//!
//! Anything beyond this subset is rejected (`ParseError::Unsupported`).
//! That's deliberate: cert-grade input parsing means **less is more**.
//!
//! ## Implementation
//!
//! Single-pass, line-oriented, with a tiny state machine to handle
//! multi-line arrays. No allocation. No panics. All output strings
//! borrow from the input byte slice.
//!
//! ## Memory cost
//!
//! Stack-resident scratch: one `[CapSpec; MAX_CAPS_PER_TASK]` (~8 KB).
//! That buffer accumulates the caps for the *current* task and is
//! committed to the topology pool when the task's section ends.

use robot_os_abi::cap::{CapKind, CapPerms};

use crate::types::{
    CapSpec, ClassSpec, MaybeStr, PolicyKind, Preemption, SchedConfig, Topology,
};
use crate::AdmissionError;

/// Errors returned by the parser.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParseError {
    /// Section header missing closing `]`.
    UnterminatedSection,
    /// Section header refers to an unknown top-level kind.
    UnknownSection,
    /// Key-value pair has no `=`.
    MissingEquals,
    /// Value cannot be parsed.
    BadValue,
    /// Value's type does not match the field's expected type.
    TypeMismatch,
    /// Field name is not in the section's schema.
    UnknownField,
    /// String literal not closed.
    UnterminatedString,
    /// Inline table not closed.
    UnterminatedInlineTable,
    /// Array of inline tables not closed.
    UnterminatedArray,
    /// Inline table is missing a required field.
    MissingField,
    /// Inline table field count exceeds [`MAX_INLINE_FIELDS`].
    TooManyInlineFields,
    /// More caps for one task than [`MAX_CAPS_PER_TASK_BUF`].
    TooManyCapsPerTask,
    /// The literal string used for an enum-typed field is unknown.
    UnknownEnumValue,
    /// Identifier or string longer than the configured maximum.
    NameTooLong,
    /// Encountered something the subset does not accept (floats,
    /// multi-section keys, multi-line strings, …).
    Unsupported,
    /// Admission error surfaced from the topology builder.
    Admission(AdmissionError),
}

impl From<AdmissionError> for ParseError {
    fn from(e: AdmissionError) -> Self {
        ParseError::Admission(e)
    }
}

// ──────────────────────────────────────────────────────────────────────────
// Tiny lexer helpers (operate on byte slices)
// ──────────────────────────────────────────────────────────────────────────

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t')
}

#[inline]
fn is_eol(b: u8) -> bool {
    matches!(b, b'\n' | b'\r')
}

#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

#[inline]
fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// Skip leading spaces and tabs (not newlines).
fn skip_inline_ws(input: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < input.len() && is_ws(input[i]) {
        i += 1;
    }
    &input[i..]
}

/// Skip leading whitespace (incl. newlines, comments).
fn skip_ws_and_comments(mut input: &[u8]) -> &[u8] {
    loop {
        // Inline whitespace + newlines.
        let mut i = 0;
        while i < input.len() && (is_ws(input[i]) || is_eol(input[i])) {
            i += 1;
        }
        input = &input[i..];
        if input.is_empty() || input[0] != b'#' {
            return input;
        }
        // Comment — skip to next newline.
        let mut j = 0;
        while j < input.len() && !is_eol(input[j]) {
            j += 1;
        }
        input = &input[j..];
    }
}

/// Read one logical line (until first `\n`), returning (line, rest).
/// The newline is consumed in `rest`. Strips trailing `\r`.
fn take_line(input: &[u8]) -> (&[u8], &[u8]) {
    let mut i = 0;
    while i < input.len() && input[i] != b'\n' {
        i += 1;
    }
    let mut line = &input[..i];
    if let Some((&last, rest)) = line.split_last() {
        if last == b'\r' {
            line = rest;
        }
    }
    let next = if i < input.len() { &input[i + 1..] } else { &input[i..] };
    (line, next)
}

/// Parse a quoted string. Returns `(content, after)`. No escape
/// processing for the supported subset (RFC-0005 fields are simple
/// ASCII). Rejects strings longer than `max_len`.
fn parse_quoted_string<'a>(input: &'a [u8], max_len: usize) -> Result<(&'a [u8], &'a [u8]), ParseError> {
    if input.is_empty() || input[0] != b'"' {
        return Err(ParseError::BadValue);
    }
    let body = &input[1..];
    let mut i = 0;
    while i < body.len() && body[i] != b'"' {
        if body[i] == b'\\' || is_eol(body[i]) {
            return Err(ParseError::Unsupported);
        }
        i += 1;
    }
    if i >= body.len() {
        return Err(ParseError::UnterminatedString);
    }
    if i > max_len {
        return Err(ParseError::NameTooLong);
    }
    Ok((&body[..i], &body[i + 1..]))
}

/// Parse a non-negative decimal integer. Returns `(value, after)`.
/// Negative numbers not supported (not needed in our subset).
fn parse_unsigned_int(input: &[u8]) -> Result<(u64, &[u8]), ParseError> {
    let mut i = 0;
    let mut acc: u64 = 0;
    let mut any = false;
    while i < input.len() && input[i].is_ascii_digit() {
        any = true;
        let d = (input[i] - b'0') as u64;
        acc = acc.checked_mul(10).and_then(|v| v.checked_add(d)).ok_or(ParseError::BadValue)?;
        i += 1;
    }
    if !any {
        return Err(ParseError::BadValue);
    }
    Ok((acc, &input[i..]))
}

/// Parse `true` / `false`. Returns `(value, after)`.
fn parse_bool(input: &[u8]) -> Result<(bool, &[u8]), ParseError> {
    if input.starts_with(b"true") {
        Ok((true, &input[4..]))
    } else if input.starts_with(b"false") {
        Ok((false, &input[5..]))
    } else {
        Err(ParseError::BadValue)
    }
}

/// Parse `[lo, hi]` integer range (used for `priority_range`).
fn parse_range(input: &[u8]) -> Result<(u8, u8, &[u8]), ParseError> {
    let r = skip_inline_ws(input);
    if !r.starts_with(b"[") {
        return Err(ParseError::BadValue);
    }
    let r = skip_inline_ws(&r[1..]);
    let (lo, r) = parse_unsigned_int(r)?;
    let r = skip_inline_ws(r);
    if !r.starts_with(b",") {
        return Err(ParseError::BadValue);
    }
    let r = skip_inline_ws(&r[1..]);
    let (hi, r) = parse_unsigned_int(r)?;
    let r = skip_inline_ws(r);
    if !r.starts_with(b"]") {
        return Err(ParseError::BadValue);
    }
    if lo > 255 || hi > 255 {
        return Err(ParseError::BadValue);
    }
    Ok((lo as u8, hi as u8, &r[1..]))
}

/// Take an identifier from the head of input.
fn take_ident(input: &[u8]) -> Result<(&[u8], &[u8]), ParseError> {
    if input.is_empty() || !is_ident_start(input[0]) {
        return Err(ParseError::BadValue);
    }
    let mut i = 1;
    while i < input.len() && is_ident_continue(input[i]) {
        i += 1;
    }
    Ok((&input[..i], &input[i..]))
}

// ──────────────────────────────────────────────────────────────────────────
// Section detection
// ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section<'a> {
    Class(&'a [u8]),
    Task(&'a [u8]),
    Sched,
}

/// If `line` is a section header `[...]`, return the parsed Section.
fn parse_section_line(line: &[u8]) -> Result<Option<Section<'_>>, ParseError> {
    let trimmed = skip_inline_ws(line);
    if trimmed.is_empty() || trimmed[0] != b'[' {
        return Ok(None);
    }
    // Find closing ']' on the same line.
    let body = &trimmed[1..];
    let mut close = 0;
    while close < body.len() && body[close] != b']' {
        close += 1;
    }
    if close >= body.len() {
        return Err(ParseError::UnterminatedSection);
    }
    let inner = &body[..close];
    // Whatever follows ']' must be only ws / comment.
    let trailing = skip_inline_ws(&body[close + 1..]);
    if !trailing.is_empty() && trailing[0] != b'#' {
        return Err(ParseError::Unsupported);
    }

    // Parse `class.NAME` / `task.NAME` / `sched`.
    let inner = skip_inline_ws(inner);
    if let Some(name) = strip_prefix(inner, b"class.") {
        let name = trim_trailing_ws(name);
        if name.is_empty() || name.len() > crate::types::MAX_TASK_NAME_LEN {
            return Err(ParseError::NameTooLong);
        }
        Ok(Some(Section::Class(name)))
    } else if let Some(name) = strip_prefix(inner, b"task.") {
        let name = trim_trailing_ws(name);
        if name.is_empty() || name.len() > crate::types::MAX_TASK_NAME_LEN {
            return Err(ParseError::NameTooLong);
        }
        Ok(Some(Section::Task(name)))
    } else if trim_trailing_ws(inner) == b"sched" {
        Ok(Some(Section::Sched))
    } else {
        Err(ParseError::UnknownSection)
    }
}

fn strip_prefix<'a>(s: &'a [u8], prefix: &[u8]) -> Option<&'a [u8]> {
    if s.len() >= prefix.len() && &s[..prefix.len()] == prefix {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn trim_trailing_ws(s: &[u8]) -> &[u8] {
    let mut end = s.len();
    while end > 0 && is_ws(s[end - 1]) {
        end -= 1;
    }
    &s[..end]
}

// ──────────────────────────────────────────────────────────────────────────
// Inline-table parsing (for caps array)
// ──────────────────────────────────────────────────────────────────────────

/// One inline-table field as a (key, value) pair, used during parsing.
///
/// Some variants are unused today but exist for forward compatibility
/// with future inline-table fields (numeric thresholds, bool gates).
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
enum InlineValue<'a> {
    Str(&'a [u8]),
    Int(u64),
    Bool(bool),
    Range(u8, u8),
}

/// Maximum fields per inline table — over-provisioned for forward-compat.
const MAX_INLINE_FIELDS: usize = 8;

/// Parse one `{ k = v, k = v, ... }` block. Returns the consumed bytes.
fn parse_inline_table<'a>(
    input: &'a [u8],
) -> Result<([(MaybeStr<'a>, InlineValue<'a>); MAX_INLINE_FIELDS], usize, &'a [u8]), ParseError>
{
    let r = skip_inline_ws(input);
    if r.is_empty() || r[0] != b'{' {
        return Err(ParseError::BadValue);
    }
    let mut r = &r[1..];
    let mut fields: [(MaybeStr<'a>, InlineValue<'a>); MAX_INLINE_FIELDS] =
        [(MaybeStr::from_bytes(&[]), InlineValue::Bool(false)); MAX_INLINE_FIELDS];
    let mut len = 0usize;
    loop {
        r = skip_inline_ws(r);
        if r.is_empty() {
            return Err(ParseError::UnterminatedInlineTable);
        }
        if r[0] == b'}' {
            return Ok((fields, len, &r[1..]));
        }
        if len >= MAX_INLINE_FIELDS {
            return Err(ParseError::TooManyInlineFields);
        }
        let (key_bytes, after) = take_ident(r)?;
        if key_bytes.len() > crate::types::MAX_TASK_NAME_LEN {
            return Err(ParseError::NameTooLong);
        }
        let after = skip_inline_ws(after);
        if !after.starts_with(b"=") {
            return Err(ParseError::MissingEquals);
        }
        let after = skip_inline_ws(&after[1..]);
        // Value: string | int | bool
        let (value, after) = if after.starts_with(b"\"") {
            let (s, rest) = parse_quoted_string(after, crate::types::MAX_TARGET_LEN)?;
            (InlineValue::Str(s), rest)
        } else if after.starts_with(b"true") || after.starts_with(b"false") {
            let (b, rest) = parse_bool(after)?;
            (InlineValue::Bool(b), rest)
        } else {
            let (n, rest) = parse_unsigned_int(after)?;
            (InlineValue::Int(n), rest)
        };
        fields[len] = (MaybeStr::from_bytes(key_bytes), value);
        len += 1;
        let after = skip_inline_ws(after);
        if after.starts_with(b",") {
            r = &after[1..];
            continue;
        }
        if after.starts_with(b"}") {
            return Ok((fields, len, &after[1..]));
        }
        return Err(ParseError::UnterminatedInlineTable);
    }
}

/// Convert a parsed inline table into a `CapSpec`.
fn cap_spec_from_inline<'a>(
    fields: &[(MaybeStr<'a>, InlineValue<'a>)],
) -> Result<CapSpec<'a>, ParseError> {
    let mut kind: Option<CapKind> = None;
    let mut perms: Option<CapPerms> = None;
    let mut target: Option<MaybeStr<'a>> = None;
    for (k, v) in fields {
        match k.as_str() {
            "kind" => {
                let s = match v {
                    InlineValue::Str(s) => *s,
                    _ => return Err(ParseError::TypeMismatch),
                };
                kind = Some(parse_cap_kind(s)?);
            }
            "perm" | "perms" => {
                let s = match v {
                    InlineValue::Str(s) => *s,
                    _ => return Err(ParseError::TypeMismatch),
                };
                perms = Some(parse_cap_perms(s)?);
            }
            "target" | "resource" => {
                let s = match v {
                    InlineValue::Str(s) => *s,
                    _ => return Err(ParseError::TypeMismatch),
                };
                target = Some(MaybeStr::from_bytes(s));
            }
            _ => return Err(ParseError::UnknownField),
        }
    }
    let kind = kind.ok_or(ParseError::MissingField)?;
    let perms = perms.unwrap_or(CapPerms::READ);
    let target = target.unwrap_or(MaybeStr::from_bytes(&[]));
    Ok(CapSpec { kind, perms, target })
}

fn parse_cap_kind(s: &[u8]) -> Result<CapKind, ParseError> {
    let s = core::str::from_utf8(s).map_err(|_| ParseError::Unsupported)?;
    let k = match s {
        "channel" | "channel-pub" | "channel-sub" => CapKind::Channel,
        "shm" => CapKind::Shm,
        "port" => CapKind::Port,
        "irq" => CapKind::Irq,
        "mmio" | "mmio-region" => CapKind::MmioRegion,
        "io-ring" => CapKind::IoRing,
        "sensor" | "encoder" => CapKind::Sensor,
        "gpio" => CapKind::Gpio,
        "i2c" => CapKind::I2c,
        "pwm" => CapKind::Pwm,
        "motor" => CapKind::Motor,
        "file" => CapKind::File,
        "socket" => CapKind::Socket,
        "task" => CapKind::Task,
        "ai-session" | "service-call" => CapKind::AiSession,
        _ => return Err(ParseError::UnknownEnumValue),
    };
    Ok(k)
}

fn parse_cap_perms(s: &[u8]) -> Result<CapPerms, ParseError> {
    let mut p = CapPerms::NONE;
    for &b in s {
        p = match b {
            b'r' | b'R' => p.union(CapPerms::READ),
            b'w' | b'W' => p.union(CapPerms::WRITE),
            b'x' | b'X' => p.union(CapPerms::EXEC),
            b'd' | b'D' => p.union(CapPerms::DUP),
            _ => return Err(ParseError::UnknownEnumValue),
        };
    }
    Ok(p)
}

// ──────────────────────────────────────────────────────────────────────────
// Public entry points
// ──────────────────────────────────────────────────────────────────────────

/// Maximum caps in the per-task scratch buffer. RFC-0003 sets the
/// upper bound at 256 (matches `robot_os_ipc::cap::MAX_CAPS_PER_TASK`),
/// but the topology pool is bounded separately to `MAX_CAPS_TOTAL`.
const MAX_CAPS_PER_TASK_BUF: usize = 256;

/// Parse `CAPS.TOML` content into the topology.
///
/// The topology may already contain classes parsed from `SCHED.TOML`;
/// this call appends task entries.
pub fn parse_caps<'a>(
    input: &'a [u8],
    topology: &mut Topology<'a>,
) -> Result<(), ParseError> {
    let mut current_section: Option<Section<'_>> = None;
    let mut current_task_caps: [CapSpec<'a>; MAX_CAPS_PER_TASK_BUF] =
        [CapSpec::empty(); MAX_CAPS_PER_TASK_BUF];
    let mut current_task_caps_len: usize = 0;
    let mut current_task_class: MaybeStr<'a> = MaybeStr::from_bytes(b"best_effort");
    let mut current_task_priority: u8 = 0;
    let mut current_task_name: Option<MaybeStr<'a>> = None;

    let mut rest: &'a [u8] = input;
    while !rest.is_empty() {
        let (line, next) = take_line(rest);
        rest = next;

        // Strip comment + trailing whitespace.
        let mut effective = line;
        if let Some(idx) = find_comment_start(effective) {
            effective = &effective[..idx];
        }
        let effective = trim_trailing_ws(skip_inline_ws(effective));
        if effective.is_empty() {
            continue;
        }

        // Section?
        if let Some(section) = parse_section_line(effective)? {
            // Commit previous task before switching.
            if let Some(name) = current_task_name.take() {
                topology
                    .push_task(
                        name,
                        current_task_class,
                        current_task_priority,
                        &current_task_caps[..current_task_caps_len],
                    )
                    .map_err(ParseError::Admission)?;
                current_task_caps_len = 0;
                current_task_class = MaybeStr::from_bytes(b"best_effort");
                current_task_priority = 0;
            }
            match section {
                Section::Task(name) => {
                    current_task_name = Some(MaybeStr::from_bytes(name));
                }
                Section::Class(_) | Section::Sched => {
                    // CAPS.TOML doesn't own these sections — silently
                    // skip; SCHED.TOML parser will pick them up.
                    current_task_name = None;
                }
            }
            current_section = Some(section);
            continue;
        }

        // Otherwise it's a kv line within a section.
        match current_section {
            Some(Section::Task(_)) => {
                handle_task_kv(
                    &mut rest,
                    effective,
                    &mut current_task_caps,
                    &mut current_task_caps_len,
                    &mut current_task_class,
                    &mut current_task_priority,
                )?;
            }
            // KV outside a recognised section is ignored.
            _ => {}
        }
    }

    // Commit the last task.
    if let Some(name) = current_task_name {
        topology
            .push_task(
                name,
                current_task_class,
                current_task_priority,
                &current_task_caps[..current_task_caps_len],
            )
            .map_err(ParseError::Admission)?;
    }

    Ok(())
}

/// Find the index of the first un-quoted `#` (start of a comment).
/// Returns `None` if there is no comment on the line.
fn find_comment_start(line: &[u8]) -> Option<usize> {
    let mut in_str = false;
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        if b == b'"' {
            in_str = !in_str;
        } else if b == b'#' && !in_str {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Handle one kv line inside a `[task.NAME]` section.
fn handle_task_kv<'a>(
    rest_in: &mut &'a [u8],
    line: &'a [u8],
    caps: &mut [CapSpec<'a>; MAX_CAPS_PER_TASK_BUF],
    caps_len: &mut usize,
    class_name: &mut MaybeStr<'a>,
    priority: &mut u8,
) -> Result<(), ParseError> {
    let (key, after) = take_ident(line)?;
    let after = skip_inline_ws(after);
    if !after.starts_with(b"=") {
        return Err(ParseError::MissingEquals);
    }
    let after = skip_inline_ws(&after[1..]);
    let key_str = core::str::from_utf8(key).map_err(|_| ParseError::Unsupported)?;
    match key_str {
        "caps" => {
            // Either a one-liner `[]` (empty caps) or multi-line array.
            let r = skip_inline_ws(after);
            if !r.starts_with(b"[") {
                return Err(ParseError::BadValue);
            }
            let body_after_bracket = &r[1..];
            // If the rest of the line (after `[`) closes immediately,
            // the array is empty.
            let line_tail = trim_trailing_ws(skip_inline_ws(body_after_bracket));
            if line_tail.starts_with(b"]") {
                return Ok(());
            }
            // Otherwise, accumulate inline tables across subsequent lines
            // until `]`.
            let mut r = body_after_bracket;
            // First, try to parse any inline tables already on this line.
            r = consume_inline_tables_on_line(r, caps, caps_len)?;
            // r now points at the trailing portion of the first line
            // after the inline tables we read; if it's still in array,
            // continue reading subsequent lines.
            // Walk forward until we hit ']'.
            loop {
                let r2 = skip_inline_ws(r);
                if r2.starts_with(b"]") {
                    return Ok(());
                }
                if r2.is_empty() || is_eol(r2[0]) {
                    // EOF guard. `take_line` on an exhausted input returns
                    // `(&[], &[])` — an empty line and an unmoved cursor. A
                    // `caps = [` whose `]` never arrives therefore drove this
                    // loop forever: `r` was set to `b""` below, `continue`
                    // brought us straight back here, `rest_in` never shrank,
                    // and no state changed between iterations. Nothing breaks
                    // that cycle — this is boot-time parsing, so there is no
                    // preemption, no timeout and no watchdog kick to save us;
                    // the board simply never finishes booting. A malformed
                    // topology file must be *rejected*, and a reject that the
                    // operator can see beats a silent hang. Check before
                    // calling `take_line` so the cursor is known to advance.
                    if rest_in.is_empty() {
                        return Err(ParseError::UnterminatedArray);
                    }
                    // Move to next line.
                    let (next_line, next) = take_line(*rest_in);
                    *rest_in = next;
                    let mut nl = next_line;
                    if let Some(idx) = find_comment_start(nl) {
                        nl = &nl[..idx];
                    }
                    let nl = skip_inline_ws(nl);
                    let nl = trim_trailing_ws(nl);
                    if nl.is_empty() {
                        r = b"";
                        continue;
                    }
                    if nl.starts_with(b"]") {
                        return Ok(());
                    }
                    r = consume_inline_tables_on_line(nl, caps, caps_len)?;
                    continue;
                }
                // Otherwise the line had stray characters.
                return Err(ParseError::UnterminatedArray);
            }
        }
        "class" => {
            let (s, _) = parse_quoted_string(after, crate::types::MAX_TASK_NAME_LEN)?;
            *class_name = MaybeStr::from_bytes(s);
            Ok(())
        }
        "priority" => {
            let (n, _) = parse_unsigned_int(after)?;
            if n > 255 {
                return Err(ParseError::BadValue);
            }
            *priority = n as u8;
            Ok(())
        }
        _ => Err(ParseError::UnknownField),
    }
}

/// Consume zero or more `{ ... },` blocks on a single line. Returns
/// the rest of the line after the last closing `}` (which may be
/// followed by `,` or `]` or whitespace + EOL).
fn consume_inline_tables_on_line<'a>(
    mut input: &'a [u8],
    caps: &mut [CapSpec<'a>; MAX_CAPS_PER_TASK_BUF],
    caps_len: &mut usize,
) -> Result<&'a [u8], ParseError> {
    loop {
        let r = skip_inline_ws(input);
        if r.is_empty() || r[0] != b'{' {
            return Ok(r);
        }
        let (fields, n_fields, after) = parse_inline_table(r)?;
        let cap = cap_spec_from_inline(&fields[..n_fields])?;
        if *caps_len >= MAX_CAPS_PER_TASK_BUF {
            return Err(ParseError::TooManyCapsPerTask);
        }
        caps[*caps_len] = cap;
        *caps_len += 1;
        let after = skip_inline_ws(after);
        if after.starts_with(b",") {
            input = &after[1..];
            continue;
        }
        return Ok(after);
    }
}

/// Parse `SCHED.TOML` content into the topology.
pub fn parse_sched<'a>(
    input: &'a [u8],
    topology: &mut Topology<'a>,
) -> Result<(), ParseError> {
    let mut current_section: Option<Section<'_>> = None;
    let mut current_class_name: Option<MaybeStr<'a>> = None;
    let mut staged = ClassSpec::empty();
    let mut sched_cfg = SchedConfig::DEFAULT;

    let mut rest: &'a [u8] = input;
    while !rest.is_empty() {
        let (line, next) = take_line(rest);
        rest = next;
        let mut effective = line;
        if let Some(idx) = find_comment_start(effective) {
            effective = &effective[..idx];
        }
        let effective = trim_trailing_ws(skip_inline_ws(effective));
        if effective.is_empty() {
            continue;
        }
        if let Some(section) = parse_section_line(effective)? {
            // Commit previous class if applicable.
            if let Some(name) = current_class_name.take() {
                staged.name = name;
                topology
                    .push_class(staged)
                    .map_err(ParseError::Admission)?;
                staged = ClassSpec::empty();
            }
            match section {
                Section::Class(name) => {
                    current_class_name = Some(MaybeStr::from_bytes(name));
                }
                Section::Sched | Section::Task(_) => {
                    current_class_name = None;
                }
            }
            current_section = Some(section);
            continue;
        }
        match current_section {
            Some(Section::Class(_)) => {
                handle_class_kv(effective, &mut staged)?;
            }
            Some(Section::Sched) => {
                handle_sched_kv(effective, &mut sched_cfg)?;
            }
            _ => {}
        }
    }
    if let Some(name) = current_class_name {
        staged.name = name;
        topology
            .push_class(staged)
            .map_err(ParseError::Admission)?;
    }
    topology.set_sched_config(sched_cfg);
    Ok(())
}

fn handle_class_kv<'a>(line: &'a [u8], staged: &mut ClassSpec<'a>) -> Result<(), ParseError> {
    let (key, after) = take_ident(line)?;
    let after = skip_inline_ws(after);
    if !after.starts_with(b"=") {
        return Err(ParseError::MissingEquals);
    }
    let after = skip_inline_ws(&after[1..]);
    let key = core::str::from_utf8(key).map_err(|_| ParseError::Unsupported)?;
    match key {
        "cpu_budget_min_pct" => {
            let (n, _) = parse_unsigned_int(after)?;
            if n > 100 {
                return Err(ParseError::BadValue);
            }
            staged.cpu_budget_min_pct = n as u8;
        }
        "cpu_budget_max_pct" => {
            let (n, _) = parse_unsigned_int(after)?;
            if n > 100 {
                return Err(ParseError::BadValue);
            }
            staged.cpu_budget_max_pct = n as u8;
        }
        "policy" => {
            let (s, _) = parse_quoted_string(after, 16)?;
            let s = core::str::from_utf8(s).map_err(|_| ParseError::Unsupported)?;
            staged.policy = PolicyKind::from_str(s).ok_or(ParseError::UnknownEnumValue)?;
        }
        "priority_range" => {
            let (lo, hi, _) = parse_range(after)?;
            staged.priority_range = (lo, hi);
        }
        "preemption" => {
            let (s, _) = parse_quoted_string(after, 16)?;
            let s = core::str::from_utf8(s).map_err(|_| ParseError::Unsupported)?;
            staged.preemption = Preemption::from_str(s).ok_or(ParseError::UnknownEnumValue)?;
        }
        "time_slice_ms" => {
            let (n, _) = parse_unsigned_int(after)?;
            if n > u16::MAX as u64 {
                return Err(ParseError::BadValue);
            }
            staged.time_slice_ms = n as u16;
        }
        "admission_control" => {
            let (b, _) = parse_bool(after)?;
            staged.admission_control = b;
        }
        _ => return Err(ParseError::UnknownField),
    }
    Ok(())
}

fn handle_sched_kv(line: &[u8], cfg: &mut SchedConfig) -> Result<(), ParseError> {
    let (key, after) = take_ident(line)?;
    let after = skip_inline_ws(after);
    if !after.starts_with(b"=") {
        return Err(ParseError::MissingEquals);
    }
    let after = skip_inline_ws(&after[1..]);
    let key = core::str::from_utf8(key).map_err(|_| ParseError::Unsupported)?;
    match key {
        "partition_window_us" => {
            let (n, _) = parse_unsigned_int(after)?;
            if n > u32::MAX as u64 {
                return Err(ParseError::BadValue);
            }
            cfg.partition_window_us = n as u32;
        }
        _ => return Err(ParseError::UnknownField),
    }
    Ok(())
}

// Avoid unused-import warning when `current_section` is only set, never
// read in a code path the compiler cares about.
#[allow(dead_code)]
fn _unused_section_ref(_s: Section<'_>) {}

#[allow(dead_code)]
fn _unused_skip(_input: &[u8]) -> &[u8] {
    skip_ws_and_comments(_input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Topology;

    /// A `caps = [` array that runs off the end of the file must be
    /// *rejected*, not hung on. Before the EOF guard in `handle_task_kv`
    /// this input span the multi-line-array loop forever: `take_line` on
    /// an exhausted cursor keeps returning an empty line without
    /// advancing, so the loop had no way to terminate. If this test ever
    /// stops returning, the guard was removed — the failure mode is a
    /// hang at boot, so the test times out rather than failing loudly.
    #[test]
    fn unterminated_caps_array_is_rejected_not_hung() {
        let mut topo = Topology::empty();
        let caps = b"[task.a]\ncaps = [\n";
        assert_eq!(parse_caps(caps, &mut topo), Err(ParseError::UnterminatedArray));
    }

    /// Same hazard with no trailing newline at all: `take_line` returns
    /// the final partial line and an empty remainder, so the very next
    /// iteration hits the guard.
    #[test]
    fn unterminated_caps_array_without_trailing_newline_is_rejected() {
        let mut topo = Topology::empty();
        let caps = b"[task.a]\ncaps = [ { kind = \"motor\", target = \"motor.0\", perm = \"rw\" },";
        assert_eq!(parse_caps(caps, &mut topo), Err(ParseError::UnterminatedArray));
    }

    /// The guard must not reject well-formed multi-line arrays: the
    /// closing `]` arrives on its own line, several blank/comment lines
    /// after the last inline table.
    #[test]
    fn multi_line_caps_array_still_parses() {
        let mut topo = Topology::empty();
        let caps = b"[task.a]\ncaps = [\n  { kind = \"motor\", target = \"motor.0\", perm = \"rw\" },\n\n  # trailing comment\n]\n";
        assert!(parse_caps(caps, &mut topo).is_ok());
        assert_eq!(topo.tasks_len(), 1);
    }
}
