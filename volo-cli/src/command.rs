use crate::context::Context;

pub trait CliCommand {
    fn run(&self, cx: Context) -> anyhow::Result<()>;
}

macro_rules! define_commands {
    {$name: ident {
        $($command:ident),+
    }} => {
        #[derive(Parser, Debug)]
        enum $name {
            $(
                $command($command),
            )*
        }

        impl CliCommand for $name {
            fn run(&self, cx: $crate::context::Context) -> anyhow::Result<()> {
                match self {
                    $(
                        $name::$command(c) => c.run(cx),
                    )*
                }
            }
        }
    };
}
