# Contributing to Shell-Chain

感谢你考虑为 Shell-Chain 做贡献！以下是参与指南。

## 开发环境

### 前置要求

- Rust 1.75+（`rustup update stable`）
- C 编译器（pqcrypto 原生绑定需要）
- Git

### 初始化

```bash
git clone https://github.com/LucienSong/shell-chain.git
cd shell-chain
cargo build
cargo test
```

## 开发流程

我们使用 **Feature-Driven Development (FDD)** 方法论。每个功能以 Feature 为单位组织。

### 分支策略

| 分支 | 用途 |
|------|------|
| `main` | 稳定版本，受保护 |
| `feat/<feature-id>` | Feature 开发分支 |
| `fix/<issue-id>` | Bug 修复分支 |

### 提交规范

使用 [Conventional Commits](https://www.conventionalcommits.org/)：

```
feat(primitives): add BLAKE3 hash function
fix(crypto): zeroize secret key on drop
docs(readme): update crate status table
test(core): add block RLP roundtrip test
refactor(core): split Signer into Signer + Verifier
```

格式：`<type>(<scope>): <description>`

**Type**: `feat`, `fix`, `docs`, `test`, `refactor`, `chore`, `ci`
**Scope**: crate 名称或模块（`primitives`, `crypto`, `core`, `storage` 等）

### Pull Request 流程

1. 从 `main` 创建 feature 分支
2. 实现功能，确保通过所有测试
3. 提交 PR，填写模板
4. 等待 Code Review
5. 合并后删除 feature 分支

## 代码规范

### Rust 风格

- 遵循 `rustfmt` 默认配置
- 遵循 `clippy` 建议（`cargo clippy --workspace`）
- 公共 API 必须有文档注释（`///`）
- 不在代码中添加多余注释，代码应当自解释

### 测试

- 每个模块包含单元测试（`#[cfg(test)] mod tests`）
- 集成测试放在 `tests/` 目录
- 新功能必须有测试覆盖
- 运行全部测试：`cargo test --workspace`

### 安全相关

- 私钥材料必须使用 `zeroize` 保证 drop 时清零
- 不引入非量子安全的密码学原语（除非明确标记为 deprecated 兼容层）
- 签名验证代码必须有负面测试（错误签名、错误公钥）

## 架构概览

```
shell-primitives  ←  shell-crypto  ←  shell-core
       ↑                  ↑               ↑
       └──────────────────┼───────────────┤
                          │         ┌─────┴──────┐
                       storage    evm    consensus
                          │        │         │
                       network ← mempool     │
                          │                  │
                        node ← rpc ──────────┘
```

详细设计见上游仓库 `shell-dev/plans/harness-design.md`。

## 报告问题

使用 [GitHub Issues](https://github.com/LucienSong/shell-chain/issues)，请包含：

- 问题的清晰描述
- 复现步骤
- 预期行为 vs 实际行为
- Rust 版本和操作系统

## License

贡献的代码将以 [MIT License](LICENSE) 发布。
