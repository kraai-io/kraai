{
  pkgs,
  sandboxTests,
  runtimeTests,
}:
pkgs.testers.runNixOSTest {
  name = "kraai-sandbox";
  nodes.machine = {
    users.users.sandbox = {
      isNormalUser = true;
      createHome = true;
    };
    environment.systemPackages = [pkgs.bubblewrap];
    virtualisation.memorySize = 2048;
    virtualisation.cores = 2;
  };
  testScript = ''
    import re
    import shlex

    start_all()
    machine.wait_for_unit("multi-user.target")

    def run_as_user(command):
        return "su - sandbox -c " + shlex.quote(command)

    def run_test(binary, test_filter):
        command = f"{binary} {test_filter} --nocapture --test-threads=1 2>&1"
        output = machine.succeed(run_as_user(command), timeout=180)
        assert re.search(r"test result: ok\. [1-9][0-9]* passed", output), output
        return output

    with subtest("unprivileged permission and network matrix"):
        machine.succeed(run_as_user("test $(id -u) -ne 0"))
        run_test("${sandboxTests}/tests/capabilities", "capability_matrix")
        run_test("${sandboxTests}/tests/capabilities", "linked_metadata_obeys_the_same_capabilities")

    with subtest("Nushell host accepts a symlinked workspace"):
        run_test("${runtimeTests}/tests/host_execution", "--exact sandboxed_host_accepts_a_workspace_symlink_alias")

    with subtest("process groups stop on exit, timeout, cancellation and drop"):
        run_test("${sandboxTests}/tests/kraai_sandbox-*", "kills_process_group_descendants")

    with subtest("disabled user namespaces fail closed"):
        machine.succeed("sysctl -w user.max_user_namespaces=0")
        status, output = machine.execute(run_as_user(
            "${sandboxTests}/tests/capabilities --exact capability_matrix::workspace_read::offline --nocapture 2>&1"
        ), timeout=60)
        assert status != 0, output
        assert "SandboxUnavailable" in output, output
        run_test("${sandboxTests}/tests/capabilities", "--exact capability_matrix::unsandboxed")
        machine.succeed("sysctl -w user.max_user_namespaces=1024")
        run_test("${sandboxTests}/tests/capabilities", "--exact capability_matrix::workspace_read::offline")
  '';
}
