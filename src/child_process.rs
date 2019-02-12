use std::env;
use std::ffi::CString;
use nix::unistd;
use nix::sys::ptrace;
use nix::sys::uio;
use nix::sys::wait;
use crate::syscalls;

enum ProcessFileState {
    Opened,
    Closed,
    CouldNotOpen,
}

enum ProcessFileMode {
    RO,
    RW,
    WO,
}

enum ProcessFileFlags {
    Unknown,
}

struct ProcessFileRec {
    state: ProcessFileState,
    filename: String,
    mode: ProcessFileMode,
    flags: ProcessFileFlags,
}

pub struct ProcessState {
    files: Vec<ProcessFileRec>,
}

impl ProcessState {
    pub fn new() -> ProcessState {
        Self {
            files: Vec::new(),
        }
    }
}

pub fn get_child_buffer(pid: unistd::Pid, base: usize, len: usize) -> String {
    let mut rbuf: Vec<u8> = vec![0; len];
    let remote_iovec = uio::RemoteIoVec{ base: base, len: len };
    uio::process_vm_readv(
        pid,
        &[uio::IoVec::from_mut_slice(rbuf.as_mut_slice())],
        &[remote_iovec],
    )
        .expect("Unable to read from child process virtual memory");
    String::from_utf8_lossy(&rbuf).into_owned()
}

pub fn get_child_buffer_cstr(pid: unistd::Pid, base: usize) -> String {
    let mut final_buf: Vec<u8> = Vec::with_capacity(255);

    // Current RemoteIoVec base address
    let mut current_base = base;

    // Index of 0 byte in final_buf
    let mut nul_idx: isize= -1;

    // Keep reading 255-byte chunks from the process VM until one contains a 0 byte
    // (null-termination character)
    loop {

        // Read into a temporary buffer
        let mut rbuf: Vec<u8> = vec![0; 255];
        let remote_iovec = uio::RemoteIoVec{ base: current_base, len: 255 };
        uio::process_vm_readv(
            pid,
            &[uio::IoVec::from_mut_slice(rbuf.as_mut_slice())],
            &[remote_iovec],
        )
            .expect("Unable to read from child process virtual memory");

        // Append temporary buffer to the final buffer and increase base address pointer
        final_buf.append(&mut rbuf);
        current_base += 255;

        // If final_buf contains a 0 byte, store the index and break from the read loop
        if final_buf.contains(&0) {
            if let Some(idx) = final_buf.iter().position(|&x| x == 0) {
                nul_idx = idx as isize;
            }
            break;
        }
    }
    if nul_idx > -1 {
        String::from_utf8_lossy(&final_buf[0..(nul_idx as usize)]).into_owned()
    } else {
        String::from("")
    }
}

pub fn exec_child(child_cmd: String, args: env::Args) {
    ptrace::traceme().expect("CHILD: could not enable tracing by parent (PTRACE_TRACEME failed)");

    // Build new args for child process
    let mut child_args = args.map(|v| CString::new(v).unwrap()).collect::<Vec<CString>>();
    child_args.insert(0, CString::new(child_cmd.as_str()).unwrap());

    eprintln!("CHILD: executing {} with argv {:?}...", child_cmd, child_args);
    unistd::execvp(
        &CString::new(child_cmd.as_str()).unwrap(),
        child_args.as_slice(),
    )
        .expect(&format!("unable to execute {}", &child_cmd));
}

pub fn wait_child(pid: unistd::Pid) {
    wait::waitpid(pid, None).expect(&format!("Unable to wait for child PID {}", pid));
}

pub fn child_loop(child: unistd::Pid) {
    let mut conf = syscalls::SyscallConfigMap::new();
    let mut state = ProcessState::new();
    loop {
        // Await next child syscall
        ptrace::syscall(child).expect("Unable to ask for next child syscall");
        wait_child(child);

        // Get syscall details
        let mut regs = ptrace::getregs(child).expect("Unable to get syscall registers before servicing");
        let syscall_id = regs.orig_rax;

        let handler_res = syscalls::handle_pre_syscall(&mut conf, &mut state, child, syscall_id, &mut regs);

        // Execute this child syscall
        ptrace::syscall(child).expect("Unable to execute current child syscall");
        wait_child(child);

        // Get syscall result
        match ptrace::getregs(child) {
            Ok(ref mut regs) => {
                syscalls::handle_post_syscall(handler_res, &mut state, child, syscall_id, regs);
            },
            Err(err) => {
                if err.as_errno() == Some(nix::errno::Errno::ESRCH) {
                    eprintln!("\nChild process terminated");
                    break;
                }
                eprintln!("Unable to get syscall registers after servicing");
            },
        };
    }
}