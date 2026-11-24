# Python CAD 助手

CAD 助手将自然语言需求转换为参数化 CAD-DSL。默认模式使用内置模板，并调用本地 `cad-cli` 检查生成的模型。

需要 Python 3.12+。在仓库根目录构建编译器，然后安装助手并运行。

```sh
cargo build -p cad-cli --locked
python -m pip install -e services/cad-agent
python -m cad_agent.cli --prompt "120 x 80 x 40 mm shelled control box with 2.5mm wall" --json
```

如果使用其他位置的 `cad-cli`，可以通过 `CAD_CLI_BIN` 环境变量指定其路径。输出包含生成结果和编译诊断。遇到已知的不支持功能时，助手会在编译前给出提示。
