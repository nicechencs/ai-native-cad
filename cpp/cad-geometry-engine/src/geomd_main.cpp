#include "abi_version.hpp"
#include "geomd_service.hpp"
#include "occt_kernel_plugin.hpp"

#include <cstdio>
#include <iostream>
#include <string>
#include <string_view>

#ifdef _WIN32
#  include <fcntl.h>
#  include <io.h>
#endif

namespace {

void configure_binary_stdio() {
#ifdef _WIN32
    (void)_setmode(_fileno(stdin), _O_BINARY);
    (void)_setmode(_fileno(stdout), _O_BINARY);
#endif
}

void print_usage(std::ostream& out) {
    out << "cad-geomd (ABI " << cad_geomd::CAD_GEOMD_ABI_MAJOR << "."
        << cad_geomd::CAD_GEOMD_ABI_MINOR << ")\n"
        << "Usage:\n"
        << "  cad-geomd --stdio [--workers N] [--kernel auto|analytic|occt]\n"
        << "                                    Run framed binary JSON-RPC daemon over stdio\n"
        << "  cad-geomd --capabilities [--kernel auto|analytic|occt]\n"
        << "                                    Print kernel capabilities JSON and exit\n"
        << "  cad-geomd --version               Print daemon ABI version and exit\n"
        << "  cad-geomd --help                  Show this help message\n";
}

} // namespace

int main(int argc, char** argv) {
    bool stdio_mode = false;
    bool print_caps = false;
    bool print_version = false;
    uint32_t workers = 0;
    cad_geomd::KernelBackendKind kernel_kind = cad_geomd::KernelBackendKind::Auto;

    for (int i = 1; i < argc; ++i) {
        const std::string_view arg(argv[i]);
        if (arg == "--stdio") {
            stdio_mode = true;
        } else if (arg == "--capabilities") {
            print_caps = true;
        } else if (arg == "--version") {
            print_version = true;
        } else if (arg == "--help" || arg == "-h") {
            print_usage(std::cout);
            return 0;
        } else if (arg == "--workers" && i + 1 < argc) {
            const int parsed = std::atoi(argv[++i]);
            if (parsed <= 0 || parsed > 64) {
                std::cerr << "invalid --workers value (expected 1..64)\n";
                return 2;
            }
            workers = static_cast<uint32_t>(parsed);
        } else if (arg == "--kernel" && i + 1 < argc) {
            const auto st = cad_geomd::parse_kernel_backend_kind(argv[++i], kernel_kind);
            if (!st.ok()) {
                std::cerr << st.message << "\n";
                return 2;
            }
        } else {
            std::cerr << "unknown argument: " << arg << "\n";
            print_usage(std::cerr);
            return 2;
        }
    }

    if (print_version) {
        std::cout << "cad-geomd " << cad_geomd::CAD_GEOMD_ABI_MAJOR << "."
                  << cad_geomd::CAD_GEOMD_ABI_MINOR << "\n";
        return 0;
    }

    auto kernel = cad_geomd::create_kernel_backend(kernel_kind, workers);
    cad_geomd::GeomdService service(std::move(kernel), workers);

    if (print_caps) {
        bool shutdown_flag = false;
        std::cout << service.handle_request_json(R"({"id":1,"method":"capabilities"})", shutdown_flag)
                  << "\n";
        return 0;
    }

    if (!stdio_mode) {
        print_usage(std::cerr);
        return 2;
    }

    configure_binary_stdio();
    std::ios::sync_with_stdio(false);
    std::cin.tie(nullptr);
    return service.run_framed_loop(std::cin, std::cout);
}
