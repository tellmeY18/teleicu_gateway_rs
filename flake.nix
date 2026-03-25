{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        isDarwin = pkgs.stdenv.isDarwin;

      in {
        # ── Dev shell ──────────────────────────────────────────────
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = with pkgs; [
            rustToolchain
            pkg-config
            cmake
          ];

          buildInputs = with pkgs; [
            sqlite
          ]
          ++ lib.optionals isDarwin [ libiconv ]
          ++ lib.optionals (!isDarwin) [ openssl ];

          env = pkgs.lib.optionalAttrs isDarwin {
            # Point the C toolchain at the Xcode CLI tools SDK so
            # ring / aws-lc-sys / libsqlite3-sys can find system
            # frameworks (CoreServices, Security, etc.) and libiconv.
            SDKROOT = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";
          };
        };

        # ── Production package (nix build) ─────────────────────────
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "teleicu-gateway";
          version =
            (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [ pkg-config cmake ];
          buildInputs = with pkgs; [ sqlite ]
            ++ lib.optionals isDarwin [ libiconv ]
            ++ lib.optionals (!isDarwin) [ openssl ];
        };
      }
    ) // {
      # ── NixOS module (Linux deployment only) ─────────────────────
      nixosModules.default = { config, pkgs, lib, ... }:
        let
          cfg = config.services.teleicu-gateway;
          rtsptoweb = pkgs.fetchurl {
            url =
              "https://github.com/deepch/RTSPtoWeb/releases/download/v0.0.10/RTSPtoWeb_linux_amd64.tar.gz";
            hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
          };
        in {
          options.services.teleicu-gateway = {
            enable = lib.mkEnableOption "TeleICU Gateway";
            environmentFile = lib.mkOption { type = lib.types.path; };
            rtsptowebConfigFile = lib.mkOption { type = lib.types.path; };
          };

          config = lib.mkIf cfg.enable {
            systemd.services.rtsptoweb = {
              description = "RTSPtoWeb stream server";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" ];
              serviceConfig = {
                ExecStart =
                  "${rtsptoweb}/RTSPtoWeb --config ${cfg.rtsptowebConfigFile}";
                Restart = "on-failure";
                RestartSec = "3s";
                DynamicUser = true;
              };
            };

            systemd.services.teleicu-gateway = {
              description = "TeleICU Gateway";
              wantedBy = [ "multi-user.target" ];
              after = [ "network.target" "rtsptoweb.service" ];
              serviceConfig = {
                ExecStart =
                  "${self.packages.${pkgs.system}.default}/bin/teleicu-gateway";
                EnvironmentFile = cfg.environmentFile;
                Restart = "on-failure";
                RestartSec = "5s";
                DynamicUser = true;
                StateDirectory = "teleicu-gateway";
                WorkingDirectory = "/var/lib/teleicu-gateway";
                PrivateTmp = true;
                NoNewPrivileges = true;
                ProtectSystem = "strict";
                ProtectHome = true;
                ReadWritePaths = [ "/var/lib/teleicu-gateway" ];
              };
            };
          };
        };
    };
}
