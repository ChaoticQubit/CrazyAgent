from tui_flow import clear_screen_cell, screen_text

def test_adjacent_cjk_has_no_continuation_spaces():
    assert screen_text('A日本Z'.encode(), 6, 1) == 'A日本Z'

def test_declared_title_overwrites_actual_prior_border_cells():
    raw = ('┌' + '─' * 22 + '┐\x1b[1;3H Declared 日本369 ').encode()
    assert screen_text(raw, 24, 1) == '┌─ Declared 日本369 ───┐'

def test_overwriting_wide_leading_cell_clears_its_continuation():
    raw = '日本\x1b[1;1HA'.encode()
    assert screen_text(raw, 4, 1) == 'A 本'

def test_overwriting_wide_continuation_clears_its_leading_cell():
    raw = '日本\x1b[1;2HA'.encode()
    assert screen_text(raw, 4, 1) == ' A本'

def test_wide_write_across_two_old_glyphs_clears_both_fragments():
    raw = '日本\x1b[1;2H界'.encode()
    assert screen_text(raw, 4, 1) == ' 界 '

def test_combining_marks_stay_with_previous_base_character():
    assert screen_text('éX'.encode(), 2, 1) == 'éX'
    assert screen_text('á̧B'.encode(), 2, 1) == 'á̧B'

def test_ascii_absolute_cursor_and_erase_to_end_of_line():
    raw = b'abcdef\x1b[1;3HXY\x1b[1;5H\x1b[K'
    assert screen_text(raw, 6, 1) == 'abXY  '

def test_carriage_return_newline_and_relative_cursor():
    raw = b'abc\rX\nY\x1b[1A\x1b[2CZ'
    assert screen_text(raw, 6, 2) == 'Xbc Z \n Y    '

def test_erase_display_removes_previous_cells():
    raw = '日本\x1b[2J\x1b[1;1Hok'.encode()
    assert screen_text(raw, 4, 1) == 'ok  '
def test_unpaired_continuation_trips_internal_invariant():
    try:
        clear_screen_cell({(1, 2): ""}, 1, 2)
    except AssertionError:
        return
    assert False, "unpaired wide continuation must trip its internal invariant"

def test_growing_output_renders_as_one_full_parse_at_every_split():
    raw = '\x1b[?1049h\x1b[2;3H日本\x1b[1;1Hab\x1b(Bc\x1b[Kȩ́\x1b[2J\x1b[1;2Hok\r\nZ'.encode()
    for split in range(len(raw) + 1):
        output = bytearray(raw[:split])
        screen_text(output, 6, 2)
        output.extend(raw[split:])
        assert screen_text(output, 6, 2) == screen_text(bytes(raw), 6, 2), split
    output = bytearray()
    for byte in raw:
        output.append(byte)
        screen_text(output, 6, 2)
    assert screen_text(output, 6, 2) == screen_text(bytes(raw), 6, 2)
    output[:] = b'new'
    assert screen_text(output, 6, 1) == 'new   '


if __name__ == '__main__':
    test_adjacent_cjk_has_no_continuation_spaces()
    test_declared_title_overwrites_actual_prior_border_cells()
    test_overwriting_wide_leading_cell_clears_its_continuation()
    test_overwriting_wide_continuation_clears_its_leading_cell()
    test_wide_write_across_two_old_glyphs_clears_both_fragments()
    test_combining_marks_stay_with_previous_base_character()
    test_ascii_absolute_cursor_and_erase_to_end_of_line()
    test_carriage_return_newline_and_relative_cursor()
    test_erase_display_removes_previous_cells()
    test_unpaired_continuation_trips_internal_invariant()
    test_growing_output_renders_as_one_full_parse_at_every_split()
    print('decoder_checks=11')
