#pragma once

#include "occt_bridge.h"

class TopoDS_Shape;

namespace occt_topology {
void extract(const TopoDS_Shape& shape, OcctTopologyResult& out);
void release(OcctTopologyResult& result);
}
