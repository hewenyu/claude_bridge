/// cask-w - 同步发送消息到 Codex 并等待回复
///
/// 使用方式：
///   cask-w "你的问题"
///   cask-w --timeout 60 "你的问题"
use clap::Parser;
use color_eyre::Result;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "cask-w",
    about = "发送消息到 Codex 并等待回复",
    long_about = "同步发送消息到 Codex AI 并等待回复。\n如果超时，可以稍后使用 cpend 命令查看回复。"
)]
struct Args {
    /// 要发送的问题或命令
    #[arg(trailing_var_arg = true, required = true, help = "要发送的问题")]
    question: Vec<String>,

    /// 超时时间（秒），0 表示无限等待
    #[arg(short, long, default_value = "30", help = "超时时间(秒), 0=无限等待")]
    timeout: u64,
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

    // 同步发送并等待
    println!("🔔 发送问题到 Codex...");

    let timeout = if args.timeout == 0 {
        None
    } else {
        Some(Duration::from_secs(args.timeout))
    };

    let timeout_display = if args.timeout == 0 {
        "无限".to_string()
    } else {
        format!("{}", args.timeout)
    };
    println!("⏳ 等待 Codex 回复 (超时 {} 秒)...", timeout_display);

    match comm
        .ask_sync(&question, timeout)
        .await
        .map_err(|e| color_eyre::eyre::eyre!("❌ 发送失败: {}", e))?
    {
        Some(reply) => {
            println!();
            println!("🤖 Codex 回复:");
            println!("{}", reply);
        }
        None => {
            println!();
            println!("⏰ Codex 未在限定时间内回复");
            println!("💡 提示: 使用 cpend 命令稍后查看回复");
        }
    }

    Ok(())
}
