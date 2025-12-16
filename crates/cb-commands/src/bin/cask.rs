/// cask - 异步发送消息到 Codex（fire-and-forget）
///
/// 使用方式：
///   cask "你的问题"
///   cask 你的问题（多个参数会自动拼接）
use clap::Parser;
use color_eyre::Result;

#[derive(Parser)]
#[command(
    name = "cask",
    about = "发送消息到 Codex (异步)",
    long_about = "异步发送消息到 Codex AI，不等待回复。\n使用 cpend 命令查看最新回复。"
)]
struct Args {
    /// 要发送的问题或命令
    #[arg(trailing_var_arg = true, required = true, help = "要发送的问题")]
    question: Vec<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();
    let question = args.question.join(" ");

    if question.is_empty() {
        eprintln!("❌ 请提供问题内容");
        std::process::exit(1);
    }

    // 创建通信器
    let mut comm = cb_codex::CodexCommunicator::new()
        .map_err(|e| color_eyre::eyre::eyre!("❌ 创建通信器失败: {}", e))?;

    // 异步发送
    let marker = comm
        .ask_async(&question)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("❌ 发送失败: {}", e))?;

    println!("✅ 已发送到 Codex (标记: {}...)", &marker[..12.min(marker.len())]);
    println!("💡 提示: 使用 cpend 命令查看最新回复");

    Ok(())
}
