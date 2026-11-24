use clap::{Parser, ValueEnum};

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
pub struct Cmd {
    #[arg(short, long)]
    pub spec: Option<Specs>,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Specs {
    Host,
    Os,
    Kernel,
    Shell,
    Terminal,

    Cpu,
    Gpu,
    Ram,

    Resolution,
    Uptime,
}
