from tui_speed import STEPS, at, crumb, metrics, usable

HOME = (" [◆▪◆▪] CrazyAgent  /  Home" + " " * 60 + "2 agents running \n"
        + "─" * 100 + "\n  Projects  1\n  ▌ Speed Project\n")
LOADING = " [◆▪◆▪] CrazyAgent  /  Home\n" + "─" * 80 + "\n  Projects\n\n  ⠼ Loading projects\n"
OVERVIEW = " [◆▪◆▪] CrazyAgent  /  Home  /  Speed Project" + " " * 50 + "\n  ◫ Overview   » Conversation\n"
AGENTS = " [◆▪◆▪] CrazyAgent  /  Home  /  Speed Project  /  Agents\n  ◫ Overview\n"
SETTINGS = " [◆▪◆▪] CrazyAgent  /  Settings  /  Accounts\n  @ Accounts 0   ⊞ Pools 0\n"


def test_crumb_is_the_header_path_without_the_brand_or_the_status():
    assert crumb(HOME) == ("Home",)
    assert crumb(OVERVIEW) == ("Home", "Speed Project")
    assert crumb(SETTINGS) == ("Settings", "Accounts")


def test_crumb_of_a_screen_with_no_header_is_none():
    assert crumb("") is None
    assert crumb("  Projects\n") is None


def test_at_matches_the_path_tail_only_on_its_own_boundary():
    assert at(OVERVIEW, ("Home", "Speed Project"))
    assert at(AGENTS, ("Speed Project", "Agents"))
    assert not at(AGENTS, ("Home", "Speed Project"))
    assert not at(OVERVIEW, ("Home",))
    assert at(HOME, ("Home",))
    assert not at(HOME, ("Settings", "Accounts"))


def test_a_screen_still_loading_is_not_usable():
    assert usable(HOME, ("Home",), "Speed Project")
    assert not usable(LOADING, ("Home",), "Speed Project")
    assert not usable(HOME.replace("Speed Project", "Other"), ("Home",), "Speed Project")


def test_steps_name_one_metric_each_and_launch_comes_first():
    names = [name for name, _, _ in STEPS]
    assert names[0] == "launch"
    assert len(set(names)) == len(names)
    assert all(key for name, key, _ in STEPS[1:])


def test_metrics_name_every_step_in_ms():
    times = {name: 10.0 + index for index, (name, _, _) in enumerate(STEPS)}
    named = metrics(times)
    assert set(named) == {"tui_%s_ms" % name for name, _, _ in STEPS}
    assert named["tui_launch_ms"] == 10.0


def test_metrics_refuse_a_missing_or_negative_time():
    times = {name: 1.0 for name, _, _ in STEPS}
    times.pop("launch")
    try:
        metrics(times)
    except AssertionError:
        pass
    else:
        raise AssertionError("a missing step was accepted")
    times["launch"] = -1.0
    try:
        metrics(times)
    except AssertionError:
        pass
    else:
        raise AssertionError("a negative time was accepted")


if __name__ == "__main__":
    if not __debug__:
        raise SystemExit("TUI speed checks require Python assertions enabled")
    checks = [value for name, value in sorted(globals().items()) if name.startswith("test_")]
    for check in checks:
        check()
    print("tui_speed_checks=%d" % len(checks))
