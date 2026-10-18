# Vendored GPUI

`gpui-pre`, `gpui-pre-windows` and `gpui-pre-apple` 0.3.7, copied from crates.io (less
`.cargo-ok`) and patched in `client/Cargo.toml`'s `[patch.crates-io]`, so video is drawn by the GPU
on Windows and macOS: the published crates have the "surface" primitive end to end
(`PaintSurface`, `PrimitiveBatch::Surfaces`), but its only payload is a macOS `CVPixelBuffer`, the
DirectX renderer's `draw_surfaces` is an empty stub, and the Metal one draws full-range buffers
only (it asserts, taking the window down on anything else), with square corners. Everything else
is the published 0.3.7, byte for byte; to see the changes, diff a directory against
`~/.cargo/registry/src/index.crates.io-*/<crate>-0.3.7`.

To move to a newer GPUI: port the changes below (or drop them, if upstream has its own surfaces
by then), or remove the `[patch.crates-io]` lines and this directory to go back to the crates as
published. The client (`src/media/video.rs`) would then need its Windows video back on the CPU
path (BGRA `RenderImage`s through the sprite atlas, as Linux draws now) and its macOS tiles
without rounded corners or video-range buffers.

## gpui-pre

- `src/scene.rs`
  - `SurfaceFrame`: one video frame in CPU memory as YUV planes (`SurfaceFormat::I420` or
    `Nv12`, BT.601 limited range) in one shared `Arc<[u8]>`, with each plane's offset and stride
    (`SurfacePlane`), the stream it belongs to (`id`, so the renderer can keep that stream's
    textures) and a serial unique to the frame (so a frame already uploaded is not uploaded again
    when the window redraws for other reasons). `new` checks every plane lies inside the data,
    which the renderer relies on. `mirrored(true)` flips it left to right as it is drawn (your own
    camera). Platform-neutral; only the Windows renderer draws it.
  - `PaintSurface` gains, on Windows only, `frame: SurfaceFrame` and
    `corner_radii: Corners<ScaledPixels>` (rounded like images are). The macOS field is untouched.
  - `PaintSurface::corner_radii` is there on macOS too.
- `src/window.rs`: `Window::paint_surface(bounds, corner_radii, frame)` on Windows, and the macOS
  one takes `corner_radii` as well (`paint_surface(bounds, corner_radii, image_buffer)`): each
  snaps the bounds, takes the content mask, clamps and scales the radii, and inserts the
  `PaintSurface`.
- `src/elements/surface.rs`: `SurfaceSource::Frame(SurfaceFrame)` on Windows (with
  `From<SurfaceFrame>`), and `surface(...)` available on Windows as well as macOS, so a frame is
  drawn with `surface(frame).object_fit(ObjectFit::Contain).rounded(...)` like an `img`. Its
  rounded corners come from the element's style, on macOS as on Windows (upstream left a TODO).
  On macOS, `unsafe SurfaceSource::from_cv_pixel_buffer(*mut c_void)` takes over one reference
  to a `CVPixelBufferRef`, so the client draws pixel buffers without core-video's bindings of
  its own.
- `src/assets.rs`: `RenderImage::into_frames`, so Linux's video pictures, once the window has let
  go of them, give their buffers back for the next frames instead of each frame allocating its
  own.
- `Cargo.toml`: `core-foundation` 0.10 on macOS (already in the tree through core-video), for
  `from_cv_pixel_buffer`.

## gpui-pre-windows

- `src/directx_renderer.rs`
  - `draw_surfaces`, which was empty, draws each surface of the batch, in the batch's place in the
    scene's order (so tile labels and other overlays painted later stay on top), with its own
    pipeline (`surface_pipeline`, the usual alpha blending) and instance buffer, filled in
    `upload_scene_buffers` like every other primitive's.
  - `SurfaceTextures`: per stream (`SurfaceFrame::id`) an R8 texture per plane (R8 Y, U, V for
    I420; R8 Y and R8G8 UV for NV12) with their views, made when a stream first draws and remade
    only when its size or format changes. A frame is written (`UpdateSubresource`, from the
    frame's own strides) only if its serial is not the one already in the textures. Streams a
    render did not draw are dropped at the end of that render (the window redraws when a tile
    goes away, so an ended stream's GPU memory is freed then), and the plane views are unbound
    after each batch so nothing keeps them alive. The cache is cleared on device loss.
  - A second sampler (`surface_sampler`), linear and clamped: video frames are whole textures,
    and the sprite sampler's wrapping would bleed one edge into the other as they are filtered.
  - `ShaderModule::Surface` ("surface"), for both the runtime-compiled (debug) and the
    precompiled (release) shader paths.
- `src/shaders.hlsl`: `surface_vertex` / `surface_fragment`. The vertex shader places the bounds
  quad, clips with `SV_ClipDistance` against the content mask like the other primitives, and
  mirrors the texture coordinates when asked. The fragment shader samples the planes with linear
  filtering (the GPU's scaling replaces the CPU's), converts BT.601 limited-range YUV to RGB with
  the coefficients of libyuv's `I420ToARGB` (in 1/64ths, blue's capped at 2, so colours match what
  the client used to convert on the CPU) and rounds the corners with `quad_sdf`, as images are.
  Planes are bound to `t2`-`t4` (after the instance buffer in `t1`), the sampler to `s1`.
- `build.rs`: "surface" added to the modules fxc precompiles for release builds.

## gpui-pre-apple

The Metal renderer (`gpui-pre-macos` draws through it).

- `src/metal_renderer.rs`
  - `draw_surfaces` takes video-range (`420v`) as well as full-range (`420f`) bi-planar 4:2:0
    pixel buffers; any other format, or a buffer CoreVideo can't make textures of, is skipped and
    logged once instead of `assert_eq!`/`unwrap` panicking in the middle of a frame.
  - `SurfaceBounds` (the per-surface instance data, which cbindgen exports to the shaders) gains
    `corner_radii` and `video_range`, filled in `write_instances`; the surfaces' instance buffer
    is bound to the fragment stage as well.
  - Each pixel buffer drawn, with its two `CVMetalTexture`s, is held (`surfaces_in_flight`) until
    the command buffer has completed, in the same completion handler that gives back the instance
    buffer. Upstream let go of the textures as soon as they were encoded and of the buffer when
    the scene was dropped, so a decoder's pool could hand the buffer out again while the GPU was
    still reading it.
- `src/shaders.metal`: `surface_vertex` passes the surface's index (`surface_id`, flat) on;
  `surface_fragment` converts BT.601 Y'CbCr to RGB from full range or, when the surface is video
  range, after expanding luma 16-235 and chroma 16-240, and rounds the corners with `quad_sdf`, as
  sprites are. Mirroring isn't the renderer's: the client mirrors your own camera as it writes
  the buffer.

## Linux (gpui-pre-wgpu): not vendored

Linux draws video as BGRA `RenderImage`s (converted on the decoding thread, buffers reused,
dropped from the atlas as they are replaced). `gpui-pre-wgpu` already has the other half of a
surface path: a `surfaces` pipeline and `vs_surface`/`fs_surface` shaders sampling a Y and a CbCr
texture, but `PrimitiveBatch::Surfaces` is drawn as nothing and `PaintSurface` has no payload off
macOS and Windows. To draw YUV there as Windows does: make the `SurfaceFrame` field, `paint_surface`
and `SurfaceSource::Frame` in `gpui-pre` cover Linux too; in `wgpu_renderer.rs` keep per stream
(`SurfaceFrame::id`) an R8 Y and an RG8 UV texture, written with `queue.write_texture` only when
the frame's serial changes (I420 frames converted to NV12 on the decoding thread, so one shader
serves both), with a bind group per stream, and dropped when a render no longer draws the stream;
then add video range and corner radii to `SurfaceParams` and `fs_surface` as above.
