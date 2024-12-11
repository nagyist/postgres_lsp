//! Codegen tools. Derived from Biome's codegen

mod generate_crate;

use bpaf::Bpaf;
pub use self::generate_crate::generate_crate;

#[derive(Debug, Clone, Bpaf)]
#[bpaf(options)]
pub enum TaskCommand {
    /// Creates a new crate
    #[bpaf(command, long("new-crate"))]
    NewCrate {
        /// The name of the crate
        #[bpaf(long("name"), argument("STRING"))]
        name: String,
    },
}


