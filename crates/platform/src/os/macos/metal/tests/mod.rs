use super::*;

#[test]
fn upload_blit_readback_round_trips_on_gpu() {
    let ctx = match MetalContext::new() {
        Some(c) => c,
        None => return, // no Metal device (unlikely on macOS) → skip
    };
    let (w, h) = (4usize, 2usize);
    let src = ctx.make_texture(w, h);
    let dst = ctx.make_texture(w, h);

    let pixels: Vec<u32> =
        (0..(w * h) as u32).map(|i| 0xFF00_0000 | (i.wrapping_mul(0x0010_1010))).collect();
    ctx.upload(src, &pixels, w, h);

    // SAFETY: blit copy src→dst on the GPU and wait for completion.
    unsafe {
        let cb = msg_send![Id; ctx.queue, sel("commandBuffer")];
        let blit = msg_send![Id; cb, sel("blitCommandEncoder")];
        msg_send![(); blit, sel("copyFromTexture:toTexture:"), src => Id, dst => Id];
        msg_send![(); blit, sel("endEncoding")];
        msg_send![(); cb, sel("commit")];
        msg_send![(); cb, sel("waitUntilCompleted")];
    }

    let mut out = vec![0u32; w * h];
    let region =
        MTLRegion { origin: MTLOrigin::default(), size: MTLSize { width: w, height: h, depth: 1 } };
    // SAFETY: shared texture readback into a w*h buffer.
    unsafe {
        msg_send![(); dst, sel("getBytes:bytesPerRow:fromRegion:mipmapLevel:"),
            out.as_mut_ptr() as *mut c_void => *mut c_void, w * 4 => usize,
            region => MTLRegion, 0usize => usize];
        msg_send![(); src, sel("release")];
        msg_send![(); dst, sel("release")];
    }

    assert_eq!(out, pixels, "GPU blit must preserve pixels exactly");
}
