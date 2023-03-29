use shadow_shim_helper_rs::emulated_time::EmulatedTime;
use shadow_shim_helper_rs::syscall_types::ForeignPtr;
use syscall_logger::log_syscall;

use crate::core::worker::Worker;
use crate::host::syscall::handler::{SyscallContext, SyscallHandler};
use crate::host::syscall_types::{SyscallResult, TypedArrayForeignPtr};
use crate::utility::pod;

impl SyscallHandler {
    #[log_syscall(/* rv */ libc::c_int, /* info */ *const libc::sysinfo)]
    pub fn sysinfo(ctx: &mut SyscallContext, info_ptr: ForeignPtr) -> SyscallResult {
        // Pointer to the plugin memory where we write the result.
        let info_ptr = TypedArrayForeignPtr::new::<libc::sysinfo>(info_ptr, 1);

        // Seconds are needed for uptime.
        let seconds = Worker::current_time()
            .unwrap()
            .duration_since(&EmulatedTime::SIMULATION_START)
            .as_secs();

        // Get a zeroed struct to make sure we init all fields.
        let mut info = pod::zeroed::<libc::sysinfo>();

        // These values are chosen arbitrarily; we don't think it matters too
        // much, except to maintain determinism. For example, Tor make decisions
        // about how many circuits to allow to be open (and other OOM settings)
        // based on available memory.
        info.uptime = i64::try_from(seconds).unwrap_or(i64::MAX);
        info.loads[0] = 1;
        info.loads[1] = 1;
        info.loads[2] = 1;
        info.totalram = 32;
        info.freeram = 24;
        info.sharedram = 4;
        info.bufferram = 4;
        info.totalswap = 0;
        info.freeswap = 0;
        info.procs = 100;
        info.totalhigh = 4;
        info.freehigh = 3;
        info.mem_unit = 1024 * 1024 * 1024; // GiB

        // Write the result to plugin memory.
        ctx.objs
            .process
            .memory_borrow_mut()
            .copy_to_ptr(info_ptr, &[info])?;
        Ok(0.into())
    }
}
