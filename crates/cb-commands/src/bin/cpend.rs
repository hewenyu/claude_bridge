/// cpend - 获取 Codex 待处理的最新回复
///
/// 使用方式：
///   cpend
use clap::Parser;
use color_eyre::Result;

#[derive(Parser)]
#[command(
    name = "cpend",
    about = "获取 Codex 待处理的最新回复",
    long_about = "从 Codex 日志中获取最新的回复消息。\n适用于异步发送后查看回复，或超时后获取结果。"
)]
struct Args {}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    color_eyre::install()?;

    Args::parse();

    // 创建通信器
    let mut comm = cb_codex::CodexCommunicator::new()
        .map_err(|e| color_eyre::eyre::eyre!("❌ 创建通信器失败: {}", e))?;

    // 获取待处理回复
    match comm
        .consume_pending()
        .map_err(|e| color_eyre::eyre::eyre!("❌ 获取回复失败: {}", e))?
    {
        Some(message) => {
            println!("🤖 Codex 最新回复:");
            println!("{}", message);
        }
        None => {
            println!("💬 暂无 Codex 回复");
            println!("💡 提示: 使用 cask 命令发送问题");
        }
    }

    Ok(())
}
