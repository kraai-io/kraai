{
  pkgs,
  kraai-acp,
}:
pkgs.testers.runNixOSTest {
  name = "kraai-shared-sessions";
  nodes.machine = {
    users.users.sessions = {
      isNormalUser = true;
      createHome = true;
    };
    environment.systemPackages = [pkgs.python3];
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
  };
  testScript = ''
    import shlex

    start_all()
    machine.wait_for_unit("multi-user.target")
    with subtest("separate clients, script heartbeat, crash and paused-owner takeover"):
        command = "${pkgs.python3}/bin/python3 -u ${./tests}/shared_sessions.py ${kraai-acp}/bin/kraai-acp"
        machine.succeed("su - sessions -c " + shlex.quote(command), timeout=240)
  '';
}
