/// cping - 检查 Codex 会话健康状态
///
/// 使用方式：
///   cping
///   cping --json (输出 JSON 格式)
use clap::Parser;
use color_eyre::Result;

#[derive(Parser)]
#[command(
    name = "cping",
    about = "检查 Codex 会话健康状态",
    long_about = "检查当前 Codex 会话的健康状态，包括进程状态、通信管道等。"
)]
struct Args {
    /// 输出 JSON 格式
    #[arg(long, help = "以 JSON 格式输出详细状态")]
    json: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();

    // 创建通信器
    let comm = cb_codex::CodexCommunicator::new()
        .map_err(|e| color_eyre::eyre::eyre!("❌ 创建通信器失败: {}", e))?;

    if args.json {
        // JSON 输出
        let status = comm.get_status();
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        // 人类可读输出
        let (healthy, status) = comm
            .ping()
            .map_err(|e| color_eyre::eyre::eyre!("❌ 健康检查失败: {}", e))?;

        if healthy {
            println!("✅ Codex 连接正常 ({})", status);
            std::process::exit(0);
        } else {
            println!("❌ Codex 连接异常: {}", status);
            println!("💡 提示: 请运行 claude_bridge up codex 启动新会话");
            std::process::exit(1);
        }
    }

    Ok(())
}
