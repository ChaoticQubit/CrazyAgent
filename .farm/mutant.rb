path = ARGV.fetch(0)
s = File.read(path)
want = {
  "fn remove_home_below(" => 1,
  "pub(crate) fn open_home_folder(" => 1,
  "fn open_up<P: rustix::path::Arg + Copy>(" => 2,
}
want.each do |head, count|
  start = s.index(head) or abort "no #{head}"
  stop = s.index("\n}\n", start) or abort "no end of #{head}"
  body = s[start...stop]
  found = body.scan("AtFlags::SYMLINK_NOFOLLOW").size
  abort "#{head}: #{found} no-follow flags, want at least #{count}" if found < count
  s[start...stop] = body.gsub("AtFlags::SYMLINK_NOFOLLOW", "AtFlags::empty()")
  puts "mutant #{head} #{found} no-follow flags dropped"
end
File.write(path, s)
