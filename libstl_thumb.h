#ifndef LIBSTL_THUMB_H
#define LIBSTL_THUMB_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// Allows utilizing `stl-thumb` from C-like languages
///
/// This function renders an image of the file `model_filename_c` and stores it into the buffer `buf_ptr`.
///
/// You must provide a memory buffer large enough to store the image. Images are written in 8-bit RGBA format,
/// so the buffer must be at least `width`*`height`*4 bytes in size. `model_filename_c` is a pointer to a C string with
/// the file path.
///
/// Returns `true` if succesful and `false` if unsuccesful.
///
/// # Example in C
/// ```c
/// const char* model_filename_c = "3DBenchy.stl";
/// int width = 256;
/// int height = 256;
///
/// size_t img_size = (size_t)width * height * 4;
/// uint8_t *buf_ptr = malloc(img_size);
///
/// render_to_buffer(buf_ptr, width, height, model_filename_c);
/// ```
///
/// # Safety
///
/// * `buf_ptr` _must_ point to a buffer that is valid for writes of `width * height * 4` bytes.
/// * `model_filename_c` must point to a valid null-terminated string.
bool render_to_buffer(uint8_t *buf_ptr,
                      uint32_t width,
                      uint32_t height,
                      const char *model_filename_c);

#ifdef __cplusplus
} // extern "C"
#endif

#endif // LIBSTL_THUMB_H
