//! Takes the `amount` setting off every line tagged `plugin`, and asks for
//! a little more than the cart holds to show SOAT keeps it within.
#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::ToString;
use alloc::vec::Vec;

wit_bindgen::generate!({
    path: "../../../soat-plugin-api/wit",
    world: "discounts",
});

use soat::plugin::host::{log, setting};
use soat::plugin::types::Level;

struct Loyalty;

impl Guest for Loyalty {
    fn discount(cart: HookCart) -> Vec<Adjustment> {
        let amount: i64 = setting("amount")
            .and_then(|a| a.parse().ok())
            .unwrap_or(100);
        let mut out = Vec::new();
        for (i, line) in cart.lines.iter().enumerate() {
            if line.tags.iter().any(|t| t == "plugin") {
                out.push(Adjustment {
                    line: Some(i as u32),
                    amount_minor: amount,
                    label: "Loyalty".to_string(),
                });
            }
        }
        if cart.subtotal_minor > 0 {
            out.push(Adjustment {
                line: None,
                amount_minor: cart.subtotal_minor * 10,
                label: "Too much".to_string(),
            });
        }
        log(Level::Info, &format!("{} lines seen", cart.lines.len()));
        out
    }
}

export!(Loyalty);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    core::arch::wasm32::unreachable()
}

#[global_allocator]
static ALLOC: Bump = Bump;

/// A bump allocator: plugins live for one call.
struct Bump;

static mut NEXT: usize = 0;

unsafe impl core::alloc::GlobalAlloc for Bump {
    unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
        extern "C" {
            static __heap_base: u8;
        }
        let base = core::ptr::addr_of!(__heap_base) as usize;
        if NEXT == 0 {
            NEXT = base;
        }
        let start = (NEXT + layout.align() - 1) & !(layout.align() - 1);
        let end = start + layout.size();
        let pages = core::arch::wasm32::memory_size(0) * 65536;
        if end > pages {
            let more = (end - pages).div_ceil(65536);
            if core::arch::wasm32::memory_grow(0, more) == usize::MAX {
                return core::ptr::null_mut();
            }
        }
        NEXT = end;
        start as *mut u8
    }

    unsafe fn dealloc(&self, _: *mut u8, _: core::alloc::Layout) {}
}
