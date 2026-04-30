#include <libraw/libraw.h>

void tr_set_half_size(libraw_data_t *d, int v) { d->params.half_size = v; }
void tr_set_use_camera_wb(libraw_data_t *d, int v) { d->params.use_camera_wb = v; }
