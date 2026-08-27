#[cfg(not(test))]
use std::ptr;

#[cfg(not(test))]
use objc2_core_foundation::CFType;
use objc2_core_foundation::{CGPoint, CGRect};
#[cfg(not(test))]
use objc2_core_graphics::CGContext;
use tracing::debug;

use super::cgs_window::CgsWindow;
#[cfg(not(test))]
use super::skylight::{
    CFRelease, SLSFlushWindowContentRegion, SLSGetWindowAlpha, SLWindowContextCreate,
};
use super::skylight::{G_CONNECTION, SLSDisableUpdate, SLSReenableUpdate, SLSSetWindowAlpha};
#[cfg(not(test))]
use super::window_server;
use super::window_server::WindowServerId;
use crate::sys::cg_ok;

#[derive(Debug)]
pub struct WindowAnimationProxy {
    window: Option<CgsWindow>,
    real_window: WindowServerId,
    ax_to_cgs_offset: CGPoint,
    original_alpha: f32,
    finished: bool,
}

impl WindowAnimationProxy {
    #[cfg(test)]
    pub fn create(_: WindowServerId, _: CGRect) -> Option<Self> { None }

    #[cfg(test)]
    pub fn create_batch(requests: &[(WindowServerId, CGRect)]) -> Vec<Option<Self>> {
        requests.iter().map(|_| None).collect()
    }

    #[cfg(not(test))]
    pub fn create(real_window: WindowServerId, ax_frame: CGRect) -> Option<Self> {
        Self::create_batch(&[(real_window, ax_frame)]).pop().flatten()
    }

    #[cfg(not(test))]
    pub fn create_batch(requests: &[(WindowServerId, CGRect)]) -> Vec<Option<Self>> {
        if requests.is_empty() {
            return Vec::new();
        }

        let Some(server_infos) = requests
            .iter()
            .map(|(id, _)| window_server::get_window(*id))
            .collect::<Option<Vec<_>>>()
        else {
            return requests.iter().map(|_| None).collect();
        };
        let images = std::thread::scope(|scope| {
            let captures: Vec<_> = requests
                .iter()
                .map(|(id, _)| {
                    let id = *id;
                    scope.spawn(move || window_server::capture_window_image_full(id))
                })
                .collect();
            captures
                .into_iter()
                .map(|capture| capture.join().ok().flatten())
                .collect::<Vec<_>>()
        });
        if images.iter().any(Option::is_none) {
            return requests.iter().map(|_| None).collect();
        }

        let mut proxies = Vec::with_capacity(requests.len());
        for (((real_window, ax_frame), server_info), image) in
            requests.iter().copied().zip(server_infos).zip(images)
        {
            let image = image.expect("checked capture result");
            let Some(proxy) =
                CgsWindow::new_animation_proxy(server_info.frame).ok().and_then(|proxy| {
                    render_image(proxy.id(), server_info.frame, image.cg_image())?;
                    Some(proxy)
                })
            else {
                return requests.iter().map(|_| None).collect();
            };

            let mut original_alpha = 1.0;
            if unsafe {
                SLSGetWindowAlpha(*G_CONNECTION, real_window.as_u32(), &mut original_alpha)
            } != objc2_core_graphics::CGError::Success
            {
                original_alpha = 1.0;
            }

            proxies.push(Self {
                window: Some(proxy),
                real_window,
                ax_to_cgs_offset: CGPoint::new(
                    server_info.frame.origin.x - ax_frame.origin.x,
                    server_info.frame.origin.y - ax_frame.origin.y,
                ),
                original_alpha,
                finished: false,
            });
        }

        SLSDisableUpdate(*G_CONNECTION);
        let ready = proxies.iter().all(|proxy| {
            let ordered = proxy
                .window
                .as_ref()
                .is_some_and(|window| window.order_above(Some(proxy.real_window.as_u32())).is_ok());
            let hidden =
                unsafe { cg_ok(SLSSetWindowAlpha(*G_CONNECTION, proxy.real_window.as_u32(), 0.0)) }
                    .is_ok();
            ordered && hidden
        });
        if !ready {
            for proxy in &mut proxies {
                unsafe {
                    _ = SLSSetWindowAlpha(
                        *G_CONNECTION,
                        proxy.real_window.as_u32(),
                        proxy.original_alpha,
                    );
                }
                if let Some(window) = proxy.window.take() {
                    _ = window.order_out();
                    drop(window);
                }
                proxy.finished = true;
            }
        }
        SLSReenableUpdate(*G_CONNECTION);
        if !ready {
            return requests.iter().map(|_| None).collect();
        }

        proxies.into_iter().map(Some).collect()
    }

    pub fn set_frame(&self, frame: CGRect) {
        let frame = ax_frame_to_cgs(frame, self.ax_to_cgs_offset);
        if let Some(window) = &self.window
            && let Err(error) = window.set_shape(frame)
        {
            debug!(
                ?error,
                proxy_window = window.id(),
                "Failed to crop animation proxy"
            );
        }
    }

    pub fn finish(mut self) { self.restore_real_window(); }

    fn restore_real_window(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        SLSDisableUpdate(*G_CONNECTION);
        if let Err(error) = unsafe {
            cg_ok(SLSSetWindowAlpha(
                *G_CONNECTION,
                self.real_window.as_u32(),
                self.original_alpha,
            ))
        } {
            debug!(?error, window = ?self.real_window, "Failed to restore animated window alpha");
        }
        if let Some(proxy) = self.window.take() {
            _ = proxy.order_out();
            drop(proxy);
        }
        SLSReenableUpdate(*G_CONNECTION);
    }
}

impl Drop for WindowAnimationProxy {
    fn drop(&mut self) { self.restore_real_window(); }
}

pub struct WindowAnimationUpdate {
    active: bool,
}

impl WindowAnimationUpdate {
    pub fn new() -> Self {
        SLSDisableUpdate(*G_CONNECTION);
        Self { active: true }
    }

    pub fn set_frame(&self, proxy: &WindowAnimationProxy, frame: CGRect) { proxy.set_frame(frame); }

    pub fn commit(mut self) {
        SLSReenableUpdate(*G_CONNECTION);
        self.active = false;
    }
}

impl Drop for WindowAnimationUpdate {
    fn drop(&mut self) {
        if self.active {
            SLSReenableUpdate(*G_CONNECTION);
        }
    }
}

fn ax_frame_to_cgs(frame: CGRect, offset: CGPoint) -> CGRect {
    CGRect {
        origin: CGPoint::new(frame.origin.x + offset.x, frame.origin.y + offset.y),
        size: frame.size,
    }
}

#[cfg(not(test))]
fn render_image(window_id: u32, frame: CGRect, image: &objc2_core_graphics::CGImage) -> Option<()> {
    unsafe {
        let context = SLWindowContextCreate(*G_CONNECTION, window_id, ptr::null_mut());
        let context = context.as_ref()?;
        let bounds = CGRect::new(CGPoint::ZERO, frame.size);
        CGContext::clear_rect(Some(context), bounds);
        CGContext::draw_image(Some(context), bounds, Some(image));
        CGContext::flush(Some(context));
        _ = SLSFlushWindowContentRegion(*G_CONNECTION, window_id, ptr::null_mut());
        CFRelease(context as *const CGContext as *mut CFType);
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::CGSize;

    use super::*;

    #[test]
    fn proxy_frame_preserves_size_and_applies_only_the_coordinate_offset() {
        let frame = CGRect::new(CGPoint::new(100.0, 200.0), CGSize::new(800.0, 600.0));
        let converted = ax_frame_to_cgs(frame, CGPoint::new(10.0, -20.0));

        assert_eq!(converted, CGRect::new(CGPoint::new(110.0, 180.0), frame.size));
    }
}
