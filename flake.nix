{
  description = "Nix flake for xfr";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        lib = pkgs.lib;

        rustToolchain = with pkgs; [
          cargo
          clippy
          rustc
          rustfmt
        ];

        xfr = pkgs.rustPlatform.buildRustPackage {
          pname = "xfr";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            installShellFiles
            pkg-config
          ];

          buildInputs = with pkgs;
            lib.optionals stdenv.isDarwin [
              libiconv
              SystemConfiguration
            ];

          doCheck = true;

          postInstall = ''
            $out/bin/xfr --completions bash > xfr.bash
            $out/bin/xfr --completions fish > xfr.fish
            $out/bin/xfr --completions zsh > _xfr

            installShellCompletion --cmd xfr \
              --bash xfr.bash \
              --fish xfr.fish \
              --zsh _xfr

            install -Dm644 docs/xfr.1 $out/share/man/man1/xfr.1
          '';

          meta = with lib; {
            description = "Modern network bandwidth testing with TUI";
            homepage = "https://github.com/lance0/xfr";
            license = with licenses; [ mit asl20 ];
            mainProgram = "xfr";
            platforms = platforms.unix;
          };
        };
      in
      {
        packages = {
          default = xfr;
          xfr = xfr;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ xfr ];

          packages = with pkgs;
            rustToolchain
            ++ [
              cargo-audit
              cargo-nextest
              rust-analyzer
            ];

          shellHook = ''
            echo "xfr development shell"
            echo "Available commands: cargo build, cargo test --all-features, cargo clippy --all-features"
          '';
        };
      }
    );
}
