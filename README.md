# AI-Native CAD

AI-Native CAD 是一个实验性的参数化 CAD 项目。你可以用带单位的 CAD-DSL 描述零件，通过 Rust 命令行工具检查模型，并导出 STEP、DXF 和 SVG。项目还包含 React 工作台、Tauri 桌面端与 Python CAD 助手。

## 快速开始

安装支持 Rust 2024 Edition 的 Rust 工具链，在仓库根目录运行以下命令。

```sh
cargo build --workspace --locked
cargo run -p cad-cli -- --help
```

启动工作台需要 Node.js 22+ 和 pnpm 9。

```sh
pnpm install
pnpm dev
```

`pnpm dev` 启动 React 工作台。未接入编译服务时，普通 `.kcl` 使用带标记的演示预览。要连接真实编译器，先用 `cargo build -p cad-cli --locked` 构建；另开终端，将 `PORT` 设为 `4173` 后运行 `pnpm dev:server`，再将 React 终端的 `CAD_STUDIO_API_PROXY` 设为 `http://127.0.0.1:4173` 并运行 `pnpm dev`。Tauri 桌面工作台直接使用本地 IPC，不需要启动 Node 服务。

Python CAD 助手的用法见 [CAD 助手介绍](services/cad-agent/README.md)。

许可证采用 [Apache-2.0](LICENSE)。
