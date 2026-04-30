#include <libraw/libraw.h>

#ifndef LIBRAW_MAKE_VERSION
#define LIBRAW_MAKE_VERSION(major, minor, patch) (((major) << 16) | ((minor) << 8) | (patch))
#endif

void tr_set_half_size(libraw_data_t *d, int v) { d->params.half_size = v; }
void tr_set_use_camera_wb(libraw_data_t *d, int v) { d->params.use_camera_wb = v; }

/* --- Shot info (idata + other) --- */

const char *tr_get_make(libraw_data_t *d) { return d->idata.make; }
const char *tr_get_model(libraw_data_t *d) { return d->idata.model; }
float tr_get_iso(libraw_data_t *d) { return d->other.iso_speed; }
float tr_get_shutter(libraw_data_t *d) { return d->other.shutter; }
float tr_get_aperture(libraw_data_t *d) { return d->other.aperture; }
float tr_get_focal_len(libraw_data_t *d) { return d->other.focal_len; }

/* --- Sensor info: sizes --- */

unsigned int tr_get_raw_width(libraw_data_t *d) { return d->sizes.raw_width; }
unsigned int tr_get_raw_height(libraw_data_t *d) { return d->sizes.raw_height; }
unsigned int tr_get_active_width(libraw_data_t *d) { return d->sizes.width; }
unsigned int tr_get_active_height(libraw_data_t *d) { return d->sizes.height; }
unsigned int tr_get_top_margin(libraw_data_t *d) { return d->sizes.top_margin; }
unsigned int tr_get_left_margin(libraw_data_t *d) { return d->sizes.left_margin; }
int tr_get_flip(libraw_data_t *d) { return d->sizes.flip; }

/* Returns 1 on success, 0 if unavailable (older libraw or out-of-range index). */
int tr_get_raw_inset_crop(libraw_data_t *d, int idx, unsigned int *out_cleft,
                          unsigned int *out_ctop, unsigned int *out_cwidth,
                          unsigned int *out_cheight) {
#if defined(LIBRAW_VERSION_NUMBER) && LIBRAW_VERSION_NUMBER >= LIBRAW_MAKE_VERSION(0, 21, 0)
    if (idx < 0 || idx >= 2) return 0;
    *out_cleft = d->sizes.raw_inset_crops[idx].cleft;
    *out_ctop = d->sizes.raw_inset_crops[idx].ctop;
    *out_cwidth = d->sizes.raw_inset_crops[idx].cwidth;
    *out_cheight = d->sizes.raw_inset_crops[idx].cheight;
    return 1;
#else
    (void)d; (void)idx;
    (void)out_cleft; (void)out_ctop; (void)out_cwidth; (void)out_cheight;
    return 0;
#endif
}

/* --- Sensor info: CFA pattern --- */

unsigned int tr_get_filters(libraw_data_t *d) { return d->idata.filters; }
const char *tr_get_cdesc(libraw_data_t *d) { return d->idata.cdesc; }
unsigned int tr_get_colors(libraw_data_t *d) { return d->idata.colors; }
int tr_get_xtrans_abs(libraw_data_t *d, int row, int col) {
    if (row < 0 || row >= 6 || col < 0 || col >= 6) return 0;
    return d->idata.xtrans_abs[row][col];
}

/* --- Sensor info: black/white levels --- */

unsigned int tr_get_black(libraw_data_t *d) { return d->color.black; }
unsigned int tr_get_cblack(libraw_data_t *d, int idx) {
    if (idx < 0 || idx >= 4) return 0;
    return d->color.cblack[idx];
}
unsigned int tr_get_maximum(libraw_data_t *d) { return d->color.maximum; }
unsigned int tr_get_data_maximum(libraw_data_t *d) { return d->color.data_maximum; }
int tr_get_linear_max(libraw_data_t *d, int idx) {
    if (idx < 0 || idx >= 4) return 0;
    /* libraw declares linear_max as `long`; truncating to int is safe for sensor
       saturation values (~16-bit). */
    return (int)d->color.linear_max[idx];
}

/* --- Sensor info: white balance --- */

float tr_get_cam_mul(libraw_data_t *d, int idx) {
    if (idx < 0 || idx >= 4) return 0.0f;
    return d->color.cam_mul[idx];
}
float tr_get_pre_mul(libraw_data_t *d, int idx) {
    if (idx < 0 || idx >= 4) return 0.0f;
    return d->color.pre_mul[idx];
}

/* --- Sensor info: color matrix (camera RGB -> XYZ) --- */

double tr_get_cam_xyz(libraw_data_t *d, int row, int col) {
    if (row < 0 || row >= 4 || col < 0 || col >= 3) return 0.0;
    return d->color.cam_xyz[row][col];
}
