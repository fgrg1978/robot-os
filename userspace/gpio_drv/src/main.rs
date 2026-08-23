//! E11.AQ3 — ring-3 userspace GPIO driver.
//!
//! Registers itself as the handler for `DRV_KIND_GPIO` with the kernel
//! driver-server (RFC-0002), then serves requests that the in-kernel
//! `UserDriverProxy` forwards: fetch → handle → reply. This is the proof that
//! a driver can run as an ordinary user process, with the kernel routing
//! `SYS_DRV_INVOKE` calls to it over the driver-server request/reply queues.
//!
//! The op handled here (`GPIO_OP_PING`) returns a fixed identifier plus an
//! echo of the first input byte, which is enough to prove the round-trip
//! end-to-end in QEMU. A production driver would additionally `drv_mmap` the
//! GPIO controller's MMIO window and read/write the pin registers — the same
//! serve loop, only with a real device access in the handler.

#![no_std]
#![no_main]

use robot_os_libsys as sys;

// ── Wire constants + structs — MUST byte-match robot_os_driver_server ───────
// (mirrored here so this excluded crate stays free of kernel deps; both sides
//  are `#[repr(C)]` so the layout is identical).

const DRV_KIND_GPIO: u32 = 0x0001;
const REQ_PAYLOAD_BYTES: usize = 64;
const REPLY_PAYLOAD_BYTES: usize = 64;

/// Op: liveness ping — reply carries [`PING_REPLY_TAG`, echo(input[0])].
const GPIO_OP_PING: u32 = 0;
/// Identifier the kernel smoke checks to confirm the reply came from us.
const PING_REPLY_TAG: u8 = 0xA5;
/// Reply status: success.
const STATUS_OK: i32 = 0;
/// Reply status: unknown op.
const STATUS_BAD_OP: i32 = -1;

#[derive(Clone, Copy)]
#[repr(C)]
struct DriverRequest {
    token: u64,
    client_tid: u32,
    op: u32,
    in_len: u16,
    out_cap: u16,
    input: [u8; REQ_PAYLOAD_BYTES],
}

#[derive(Clone, Copy)]
#[repr(C)]
struct DriverReply {
    token: u64,
    status: i32,
    out_len: u16,
    _pad: u16,
    output: [u8; REPLY_PAYLOAD_BYTES],
}

impl DriverRequest {
    const fn zeroed() -> Self {
        DriverRequest {
            token: 0,
            client_tid: 0,
            op: 0,
            in_len: 0,
            out_cap: 0,
            input: [0; REQ_PAYLOAD_BYTES],
        }
    }
}

impl DriverReply {
    const fn zeroed() -> Self {
        DriverReply {
            token: 0,
            status: 0,
            out_len: 0,
            _pad: 0,
            output: [0; REPLY_PAYLOAD_BYTES],
        }
    }
}

/// Build the reply for one request.
fn handle(req: &DriverRequest) -> DriverReply {
    let mut reply = DriverReply::zeroed();
    reply.token = req.token; // the proxy matches reply → waiter by token
    match req.op {
        GPIO_OP_PING => {
            reply.status = STATUS_OK;
            reply.output[0] = PING_REPLY_TAG;
            reply.output[1] = if req.in_len > 0 { req.input[0] } else { 0 };
            reply.out_len = 2;
        }
        _ => {
            reply.status = STATUS_BAD_OP;
            reply.out_len = 0;
        }
    }
    reply
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if sys::drv_srv_register(DRV_KIND_GPIO, 0, 0, 0) != 0 {
        sys::println(b"[gpio_drv] register FAILED");
        sys::exit(1);
    }
    sys::println(b"[gpio_drv] registered DRV_KIND_GPIO, serving");

    let mut req = DriverRequest::zeroed();
    loop {
        let rc = sys::drv_srv_fetch_request(
            DRV_KIND_GPIO,
            &mut req as *mut DriverRequest as *mut u8,
        );
        if rc == 0 {
            let reply = handle(&req);
            sys::drv_srv_reply(
                DRV_KIND_GPIO,
                &reply as *const DriverReply as *const u8,
            );
        } else {
            // No pending request — yield so we don't peg the hart or hammer the
            // driver-server spinlock (a tight poll wedged the whole system).
            // The kernel-side proxy waits long enough (PROXY_MAX_POLL_ITERS) to
            // span our reschedule latency, so a yield here is fine.
            sys::yield_now();
        }
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    sys::exit(2);
}
