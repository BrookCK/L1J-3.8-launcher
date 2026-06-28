use crate::aux;
use crate::logger::log_line;
use crate::patching::img_hover;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::WaitForSingleObject;

pub(crate) fn spawn_hover_poll(result: img_hover::HoverHookResult) {
    #[link(name = "user32")]
    extern "system" {
        fn GetAsyncKeyState(vkey: i32) -> i16;
        fn GetCursorPos(p: *mut HoverPoint) -> i32;
        fn GetClientRect(hwnd: isize, rect: *mut ClientRect) -> i32;
    }

    #[repr(C)]
    struct HoverPoint {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    struct ClientRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    const VK_F6: i32 = 0x75;
    let h_raw = result.game_handle.0 as usize;
    let cave_draw = result.cave_draw;
    let cave_blit = result.cave_blit;
    let pid = result.pid;

    std::thread::spawn(move || {
        let h = HANDLE(h_raw as *mut _);
        let hwnd = match img_hover::find_hwnd_by_pid(pid) {
            Ok(hwnd) => {
                log_line!(
                    "[stage2] img hover polling started (HWND=0x{:X}, F6 calibration)",
                    hwnd
                );
                Some(hwnd)
            }
            Err(_) => {
                log_line!("[stage2] img hover polling started (HWND unavailable, F6 calibration)");
                None
            }
        };
        let mut last_f6 = false;
        loop {
            if unsafe { WaitForSingleObject(h, 0) } == WAIT_OBJECT_0 {
                log_line!("[stage2] img hover polling stopped: game exited");
                return;
            }
            let mut pt = HoverPoint { x: 0, y: 0 };
            unsafe {
                GetCursorPos(&mut pt as *mut HoverPoint);
            }
            let pressed = unsafe { GetAsyncKeyState(VK_F6) } as u16 & 0x8000 != 0;
            if pressed && !last_f6 {
                img_hover::log_calibration(h, cave_draw, cave_blit, pt.x, pt.y);
            }
            last_f6 = pressed;
            let _ = img_hover::poll_hover_tick(h, cave_draw, cave_blit, pid, pt.x, pt.y);

            let (sw, sh) = if let Some(hwnd) = hwnd {
                let mut r = ClientRect {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                if unsafe { GetClientRect(hwnd, &mut r) } != 0 {
                    (r.right - r.left, r.bottom - r.top)
                } else {
                    (1024, 768)
                }
            } else {
                (1024, 768)
            };
            let _ = std::panic::catch_unwind(|| {
                let _ = aux::notification::on_polling_tick(h, Instant::now(), sw, sh);
            });

            std::thread::sleep(Duration::from_millis(30));
        }
    });
}
