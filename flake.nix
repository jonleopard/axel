{
  description = "Axel: agent orchestration, designed for Rex";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    systems.url = "github:nix-systems/default";
  };

  outputs = { self, nixpkgs, systems }:
    let
      lib = nixpkgs.lib;
      eachSystem = lib.genAttrs (import systems);
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      # Releases ship per commit, so the rev keeps nix-side versions distinguishable.
      version = cargoToml.workspace.package.version + "+g${self.shortRev or "dirty"}";
    in
    {
      packages = eachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          axel = pkgs.rustPlatform.buildRustPackage {
            pname = "axel";
            inherit version;
            cargoLock.lockFile = ./Cargo.lock;
            # cargo-setup-hook forces system sqlite over axel's rusqlite/bundled (nixpkgs#537108);
            # opting out is out-of-tree-only (nixpkgs-vet#309) — nixpkgs-style fix: pkg-config + sqlite.
            dontCargoUseSystemLibs = true;
            # keyring's linux path (sync-secret-service → dbus-secret-service → libdbus-sys)
            # links system libdbus via pkg-config; darwin uses apple-native and needs neither.
            nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.pkg-config ];
            buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.dbus ];
            nativeCheckInputs = [ pkgs.git ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.procps ];
            # sandbox gaps: darwin refuses setuid /bin/ps; linux binds no /usr/bin/env.
            checkFlags =
              lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
                "--skip" "suspends_and_resumes_an_isolated_process_tree"
              ]
              ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
                "--skip" "a_profile_selects_its_directory_and_ignores_inherited_auth_overrides"
              ];
            # linux: run the bin's unit suite only — the integration tests drive a
            # full session host (process tree + ACP stdio) the headless sandbox
            # can't host; upstream CI owns them on macOS.
            cargoTestFlags = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ "--bin" "axel" ];
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./rust-toolchain.toml
                ./crates
              ];
            };
            meta = {
              description = cargoToml.workspace.package.description;
              homepage = "https://axel.md";
              license = lib.licenses.asl20;
              mainProgram = "axel";
              maintainers = [ ];
            };
          };
          default = self.packages.${system}.axel;
        });

      checks = eachSystem (system: {
        axel = self.packages.${system}.axel;
        devShell = self.devShells.${system}.default;
      });

      devShells = eachSystem (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.axel ];
            packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ];
          };
        });

      overlays.default = final: prev: {
        axel = self.packages.${prev.system}.axel;
      };

      homeManagerModules.axel = { config, lib, pkgs, ... }:
        let cfg = config.programs.axel; in
        {
          options.programs.axel = {
            enable = lib.mkEnableOption "axel";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.stdenv.hostPlatform.system}.axel;
              defaultText = lib.literalExpression "self.packages.<system>.axel";
              description = "The axel package to install.";
            };
          };
          config = lib.mkIf cfg.enable {
            home.packages = [ cfg.package ];
          };
        };

      homeManagerModules.default = { pkgs, lib, ... }: {
        imports = [ self.homeManagerModules.axel ];
        programs.axel.package = lib.mkDefault self.packages.${pkgs.stdenv.hostPlatform.system}.axel;
      };
    };
}
