# AI-Native CAD

AI-Native CAD 是一个实验性的参数化 CAD 项目。你可以用带单位的 CAD-DSL 描述零件，通过 Rust 命令行工具检查模型，并导出 STEP、DXF 和 SVG。项目还包含 React 工作台、Tauri 桌面端与 Python CAD 助手。

## 快速开始

安装支持 Rust 2024 Edition 的 Rust 工具链，然后在仓库根目录运行：

```sh
cargo build --workspace --locked
cargo run -p cad-cli -- --help
```

如果要启动工作台，另需 Node.js 22+ 和 pnpm 9：

```sh
pnpm install
pnpm dev
```

构建好 `cad-cli` 后，可运行 `pnpm dev:server` 启动连接本地编译器的工作台。公开版不附带内部规范、评测模型或测试套件；请使用自己的 `.kcl` 模型。

许可证：[Apache-2.0](LICENSE)。
