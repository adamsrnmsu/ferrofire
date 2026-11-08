//! Thin libc wrappers: SysV shm, POSIX named semaphores, updates FIFO. Same calls as modelHooks.c.
//! Real-IPC testing happens in the Lima VM (macOS SysV/POSIX semantics differ); this compiles on both.

use super::contract::*;
use std::ffi::CString;
use std::io;

pub struct Shm {
    id: i32,
    pub addr: *mut u8,
    pub size: usize,
}

impl Shm {
    /// shmget(IPC_CREAT|0666) + shmat. A stale segment of a different size (previous run with another
    /// point count) is removed and recreated.
    pub fn create(key: i32, size: usize) -> io::Result<Shm> {
        let flags = libc::IPC_CREAT | 0o666;
        let mut id = unsafe { libc::shmget(key as libc::key_t, size, flags) };
        if id < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
            let old = unsafe { libc::shmget(key as libc::key_t, 0, 0o666) };
            if old >= 0 {
                unsafe { libc::shmctl(old, libc::IPC_RMID, std::ptr::null_mut()) };
            }
            id = unsafe { libc::shmget(key as libc::key_t, size, flags) };
        }
        if id < 0 {
            return Err(io::Error::last_os_error());
        }
        let addr = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if addr as isize == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Shm {
            id,
            addr: addr as *mut u8,
            size,
        })
    }

    /// Zero the segment, then copy `data` at `offset`.
    pub fn write(&self, offset: usize, data: &[u8]) {
        assert!(offset + data.len() <= self.size);
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.addr.add(offset), data.len()) };
    }

    pub fn zero(&self) {
        unsafe { std::ptr::write_bytes(self.addr, 0, self.size) };
    }

    pub fn destroy(&self) {
        unsafe {
            libc::shmdt(self.addr as *const libc::c_void);
            libc::shmctl(self.id, libc::IPC_RMID, std::ptr::null_mut());
        }
    }
}

pub struct Sem {
    p: *mut libc::sem_t,
    name: CString,
}

impl Sem {
    /// sem_open(name, O_CREAT, 0644, 0) -- no unlink first: the provider may already have created it.
    /// Any stale count from a crashed run is drained so the semaphore starts locked.
    pub fn open_locked(name: &str) -> io::Result<Sem> {
        let name = CString::new(name).unwrap();
        let p = unsafe {
            libc::sem_open(
                name.as_ptr(),
                libc::O_CREAT,
                0o644 as libc::c_uint,
                0 as libc::c_uint,
            )
        };
        if p == libc::SEM_FAILED {
            return Err(io::Error::last_os_error());
        }
        while unsafe { libc::sem_trywait(p) } == 0 {}
        Ok(Sem { p, name })
    }
    pub fn wait(&self) {
        // retry on EINTR (signals)
        while unsafe { libc::sem_wait(self.p) } != 0 {
            if io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
    }
    pub fn post(&self) {
        unsafe { libc::sem_post(self.p) };
    }
    pub fn destroy(&self) {
        unsafe {
            libc::sem_close(self.p);
            libc::sem_unlink(self.name.as_ptr());
        }
    }
}

pub struct Fifo {
    fd: i32,
}

impl Fifo {
    /// unlink + mkfifo(0666) + open O_RDONLY|O_NONBLOCK (provider opens the write end later).
    pub fn create() -> io::Result<Fifo> {
        let path = CString::new(UPDATES_FIFO).unwrap();
        unsafe {
            libc::unlink(path.as_ptr());
            if libc::mkfifo(path.as_ptr(), 0o666) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::chmod(path.as_ptr(), 0o666); // mkfifo honours umask
            let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK);
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Fifo { fd })
        }
    }
    /// Drain all complete 256-byte records currently queued.
    pub fn drain(&self) -> Vec<[u8; MAX_MSG_LEN]> {
        let mut out = vec![];
        loop {
            let mut rec = [0u8; MAX_MSG_LEN];
            let n =
                unsafe { libc::read(self.fd, rec.as_mut_ptr() as *mut libc::c_void, MAX_MSG_LEN) };
            if n <= 0 {
                return out;
            }
            out.push(rec);
        }
    }
    pub fn destroy(&self) {
        let path = CString::new(UPDATES_FIFO).unwrap();
        unsafe {
            libc::close(self.fd);
            libc::unlink(path.as_ptr());
        }
    }
}
