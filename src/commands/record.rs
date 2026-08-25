use crate::{Header, write_raw_frame};
use clap::Args;
use std::fs::OpenOptions;
use std::io::{self, BufReader, Read, Write};
use std::path::PathBuf;

#[derive(Args, Debug, Clone)]
pub struct RecordArgs {
    /// Open output file in append mode instead of overwrite mode
    #[arg(short = 'a', long = "append")]
    pub append: bool,

    /// Output file to write to (writes to stdout if omitted or '-')
    pub file: Option<String>,
}

pub fn run(args: RecordArgs) -> anyhow::Result<()> {
    let mut writer: Box<dyn Write> = match args.file.as_deref() {
        Some("-") | None => Box::new(io::stdout().lock()),
        Some(path_str) => {
            let path = PathBuf::from(path_str);
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .append(args.append)
                .truncate(!args.append)
                .open(&path)?;
            Box::new(file)
        }
    };

    let stdin = io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut buf = [0u8; 8192];

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }

        let header = Header::now(n as u32).map_err(io::Error::other)?;
        write_raw_frame(writer.as_mut(), &header, &buf[..n]).map_err(io::Error::other)?;
        writer.flush()?;
    }

    Ok(())
}
