self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.litz-audio;
  package = self.packages.${pkgs.stdenv.hostPlatform.system}.litzd;
in
{
  options.services.litz-audio.enable = lib.mkEnableOption "the per-user Litz microphone service";
  config = lib.mkIf cfg.enable {
    environment.systemPackages = [ package ];
    systemd.packages = [ package ];
    systemd.user.services.litzd.wantedBy = [ "default.target" ];
    networking.firewall.allowedUDPPorts = [
      47165
      5353
    ];
  };
}
