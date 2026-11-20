//! File and filesystem-related syscalls

use super::user_addr_valid;

const FD_STDOUT: usize = 1;

/// write buf of length `len`  to a file with `fd`
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!("kernel: sys_write");
    match fd {
        FD_STDOUT => {
            // The buffer comes from the application, so it must stay inside the
            // region that was handed to it. Without this check a bogus pointer
            // would fault the kernel while printing (and an unsupported `fd`
            // below would take the whole run down with a panic).
            if !user_addr_valid(buf as usize, len) {
                return -1;
            }
            let slice = unsafe { core::slice::from_raw_parts(buf, len) };
            match core::str::from_utf8(slice) {
                Ok(str) => {
                    print!("{}", str);
                    len as isize
                }
                Err(_) => -1,
            }
        }
        _ => -1,
    }
}
