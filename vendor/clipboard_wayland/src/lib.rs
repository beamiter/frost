// Copyright 2017 Avraham Weinstock
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::error::Error;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

pub struct Clipboard {
    context: Arc<Mutex<smithay_clipboard::Clipboard>>,
}

impl Clipboard {
    pub unsafe fn connect(display: *mut c_void) -> Clipboard {
        let context = Arc::new(Mutex::new(smithay_clipboard::Clipboard::new(
            display as *mut _,
        )));

        Clipboard { context }
    }

    /// Opt-in integration for winit's GUI-owned socket reader.
    /// The display must outlive this object; synchronous reads must run on the
    /// host reader thread outside a prepared read guard.
    pub unsafe fn connect_external_reader(display: *mut c_void) -> Clipboard {
        let context = Arc::new(Mutex::new(unsafe {
            smithay_clipboard::Clipboard::new_external_reader(display)
        }));
        Clipboard { context }
    }

    pub fn read_async(
        &self,
        primary: bool,
        max_bytes: usize,
        deadline: std::time::Instant,
        cancelled: Box<dyn Fn() -> bool + Send + 'static>,
        complete: Box<dyn FnOnce(std::io::Result<String>) + Send + 'static>,
    ) -> std::io::Result<()> {
        self.context
            .lock()
            .map_err(|_| std::io::Error::other("clipboard unavailable"))?
            .read_async(primary, max_bytes, deadline, cancelled, complete)
    }

    pub fn read(&self) -> Result<String, Box<dyn Error>> {
        Ok(self.context.lock().unwrap().load()?)
    }

    pub fn read_primary(&self) -> Result<String, Box<dyn Error>> {
        Ok(self.context.lock().unwrap().load_primary()?)
    }

    pub fn write(&mut self, data: String) -> Result<(), Box<dyn Error>> {
        self.context.lock().unwrap().store(data);

        Ok(())
    }

    pub fn write_primary(&mut self, data: String) -> Result<(), Box<dyn Error>> {
        self.context.lock().unwrap().store_primary(data);

        Ok(())
    }
}
