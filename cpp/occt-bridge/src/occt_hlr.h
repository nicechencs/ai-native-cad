#pragma once

#include "occt_bridge.h"

class TopoDS_Shape;

namespace occt_hlr {
int32_t compute(const TopoDS_Shape& shape, const double toward_eye[3], const double view_x[3],
                double deflection_mm, OcctHlrResult& out);
void release(OcctHlrResult& out);
}
