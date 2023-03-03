{
	inputs = {
		nixpkgs.url      = "github:NixOS/nixpkgs/nixos-unstable";
		rust-overlay.url = "github:oxalica/rust-overlay";
		flake-utils.url  = "github:numtide/flake-utils";
	};

	outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
		flake-utils.lib.eachDefaultSystem (system:
			let
				overlays = [ (import rust-overlay) ];
				pkgs = import nixpkgs {
					inherit system overlays;
				};
			in
			with pkgs;
			{
				devShells.default = mkShell {
					buildInputs = [
						glib
						python3
						cmake
						ninja
						pkg-config
						xz
						cpufrequtils
						cargo
						rustc
						pcre2
						bash
						clang
						libclang
						tor
						(rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
					];
					LD_LIBRARY_PATH = lib.makeLibraryPath [ libclang ];
				};
			}
		);
}
