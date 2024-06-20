use std::{cell::UnsafeCell, sync::Arc};

use tokio_util::sync::CancellationToken;
use volo::FastStr;

pub mod cpu;
pub mod mem;

pub struct Recoder {
    name: FastStr,
    cancel: CancellationToken,
    cpu: Arc<UnsafeCell<Vec<f32>>>,
    mem: Arc<UnsafeCell<Vec<u64>>>,
}

unsafe impl Sync for Recoder {}

impl Recoder {
    pub fn new(name: impl Into<FastStr>) -> Self {
        Self {
            name: name.into(),
            cancel: CancellationToken::new(),
            cpu: Default::default(),
            mem: Default::default(),
        }
    }

    pub async fn begin(&self) {
        let cpu_cancel = self.cancel.clone();
        let cpu_vec = self.cpu.clone();
        tokio::spawn(cpu::record_usage(
            unsafe { &mut *cpu_vec.get() },
            cpu_cancel,
        ));
        let mem_cancel = self.cancel.clone();
        let mem_vec = self.mem.clone();
        tokio::spawn(mem::record_usage(
            unsafe { &mut *mem_vec.get() },
            mem_cancel,
        ));
    }

    pub fn end(&self) {
        self.cancel.cancel();
    }

    pub fn report_string(&self) -> String {
        let cpu_usage = cpu::Usage::new(unsafe { &mut *self.cpu.get() });
        let mem_usage = mem::Usage::new(unsafe { &mut *self.mem.get() });
        format!(
            "[{}] CPU Usage: {}\n[{}] Mem Usage: {}\n",
            self.name, cpu_usage, self.name, mem_usage
        )
    }

    pub fn report(&self) {
        print!("{}", self.report_string());
    }
}
