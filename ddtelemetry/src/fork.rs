pub enum Fork {
    Parent(libc::pid_t),
    Child,
}

pub fn fork() -> Result<Fork, i32> {
    let res = unsafe { libc::fork() };
    match res {
        -1 => Err(-1),
        0 => Ok(Fork::Child),
        res => Ok(Fork::Parent(res)),
    }
}

pub fn getpid() -> libc::pid_t {
    unsafe { libc::getpid() }
}
