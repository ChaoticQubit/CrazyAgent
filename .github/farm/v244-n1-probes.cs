using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using Microsoft.Win32.SafeHandles;

/// <summary>
/// N1 probes for rv-sec2 #721 Q1: whether any write reaches a harness file while the pin holds it
/// open for read with read and delete sharing only. Input: a scratch folder. Output: one PROBE line
/// per case on stdout.
/// </summary>
public static class N1Probe
{
    const uint GENERIC_READ = 0x80000000, GENERIC_WRITE = 0x40000000, DELETE = 0x00010000;
    const uint READ_CONTROL = 0x00020000, SYNCHRONIZE = 0x00100000, FILE_WRITE_ATTRIBUTES = 0x100;
    const uint FILE_READ_ATTRIBUTES = 0x80;
    const uint SHARE_READ = 1, SHARE_WRITE = 2, SHARE_DELETE = 4, SHARE_ALL = 7;
    const uint CREATE_ALWAYS = 2, OPEN_EXISTING = 3, TRUNCATE_EXISTING = 5;
    const uint FILE_FLAG_OPEN_REPARSE_POINT = 0x00200000, FILE_FLAG_BACKUP_SEMANTICS = 0x02000000;
    const uint PAGE_READWRITE = 0x04, FILE_MAP_WRITE = 0x0002;
    const uint MOVEFILE_REPLACE_EXISTING = 0x1;
    const int SIZE = 65536;

    [StructLayout(LayoutKind.Sequential)]
    struct Bhfi
    {
        public uint Attributes, C1, C2, A1, A2, W1, W2, Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct UnicodeString
    {
        public ushort Length, MaximumLength;
        public IntPtr Buffer;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct ObjectAttributes
    {
        public int Length;
        public IntPtr RootDirectory, ObjectName;
        public uint Attributes;
        public IntPtr SecurityDescriptor, SecurityQualityOfService;
    }

    [StructLayout(LayoutKind.Sequential)]
    struct IoStatusBlock
    {
        public IntPtr Status, Information;
    }

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern SafeFileHandle CreateFileW(string name, uint access, uint share, IntPtr security, uint disposition, uint flags, IntPtr template);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr CreateFileMappingW(SafeFileHandle file, IntPtr security, uint protect, uint high, uint low, IntPtr name);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern IntPtr MapViewOfFile(IntPtr mapping, uint access, uint high, uint low, UIntPtr bytes);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool FlushViewOfFile(IntPtr view, UIntPtr bytes);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool UnmapViewOfFile(IntPtr view);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool MoveFileExW(string from, string to, uint flags);
    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool CreateHardLinkW(string link, string existing, IntPtr security);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool GetFileInformationByHandle(SafeFileHandle file, out Bhfi info);
    [DllImport("kernel32.dll", SetLastError = true)]
    static extern bool SetFileInformationByHandle(SafeFileHandle file, int cls, byte[] buffer, uint size);
    [DllImport("ntdll.dll")]
    static extern int NtCreateFile(out IntPtr handle, uint access, ref ObjectAttributes attributes, out IoStatusBlock status, IntPtr allocation, uint fileAttributes, uint share, uint disposition, uint options, IntPtr ea, uint eaLength);

    static string Error() { return "err" + Marshal.GetLastWin32Error(); }

    static SafeFileHandle Pin(string path)
    {
        return CreateFileW(path, GENERIC_READ | READ_CONTROL, SHARE_READ | SHARE_DELETE, IntPtr.Zero, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, IntPtr.Zero);
    }

    static string Id(SafeFileHandle file)
    {
        Bhfi info;
        if (!GetFileInformationByHandle(file, out info)) return "none";
        return info.Volume.ToString("x") + ":" + info.IndexHigh.ToString("x") + info.IndexLow.ToString("x8");
    }

    static string Hash(SafeFileHandle file)
    {
        var stream = new FileStream(new SafeFileHandle(file.DangerousGetHandle(), false), FileAccess.Read, 1, false);
        stream.Seek(0, SeekOrigin.Begin);
        using (var sha = SHA256.Create()) return BitConverter.ToString(sha.ComputeHash(stream)).Replace("-", "").Substring(0, 16);
    }

    static void Fill(string path, byte value)
    {
        var bytes = new byte[SIZE];
        for (int i = 0; i < SIZE; i++) bytes[i] = value;
        File.WriteAllBytes(path, bytes);
    }

    static string Write(SafeFileHandle file, byte value)
    {
        var stream = new FileStream(new SafeFileHandle(file.DangerousGetHandle(), false), FileAccess.Write, 1, false);
        var bytes = new byte[4096];
        for (int i = 0; i < bytes.Length; i++) bytes[i] = value;
        try { stream.Write(bytes, 0, bytes.Length); stream.Flush(); return "wrote"; }
        catch (Exception e) { return "write-failed-" + e.GetType().Name; }
    }

    static string NtOpen(string path, uint access, uint disposition)
    {
        var name = @"\??\" + path;
        var buffer = Marshal.StringToHGlobalUni(name);
        var text = new UnicodeString { Length = (ushort)(name.Length * 2), MaximumLength = (ushort)(name.Length * 2 + 2), Buffer = buffer };
        var textPointer = Marshal.AllocHGlobal(Marshal.SizeOf(typeof(UnicodeString)));
        Marshal.StructureToPtr(text, textPointer, false);
        var attributes = new ObjectAttributes { Length = Marshal.SizeOf(typeof(ObjectAttributes)), ObjectName = textPointer, Attributes = 0x40 };
        IntPtr handle;
        IoStatusBlock status;
        int result = NtCreateFile(out handle, access | SYNCHRONIZE, ref attributes, out status, IntPtr.Zero, 0x80, SHARE_ALL, disposition, 0x40 | 0x20, IntPtr.Zero, 0);
        Marshal.FreeHGlobal(textPointer);
        Marshal.FreeHGlobal(buffer);
        if (result != 0) return "nt" + ((uint)result).ToString("x8");
        string written = "";
        if ((access & GENERIC_WRITE) != 0) written = "+" + Write(new SafeFileHandle(handle, false), (byte)'C');
        CloseHandle(handle);
        return "ok" + written;
    }

    static byte[] RenameBuffer(string target, uint flags)
    {
        var name = System.Text.Encoding.Unicode.GetBytes(@"\\?\" + target);
        var buffer = new byte[24 + name.Length];
        BitConverter.GetBytes(flags).CopyTo(buffer, 0);
        BitConverter.GetBytes((uint)name.Length).CopyTo(buffer, 16);
        name.CopyTo(buffer, 20);
        return buffer;
    }

    static string Look(string path, SafeFileHandle pin, string pinnedId, string pinnedHash)
    {
        string pinBytes = Hash(pin) == pinnedHash ? "same" : "changed";
        string pathState = "missing", pathBytes = "none";
        using (var named = CreateFileW(path, GENERIC_READ, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, IntPtr.Zero))
        {
            if (!named.IsInvalid)
            {
                pathState = Id(named) == pinnedId ? "same-file" : "other-file";
                pathBytes = Hash(named) == pinnedHash ? "same" : "changed";
            }
        }
        bool landed = pinBytes == "changed" || (pathState == "same-file" && pathBytes == "changed");
        return "pin_bytes=" + pinBytes + " path=" + pathState + " path_bytes=" + pathBytes + " landed=" + (landed ? "YES" : "no");
    }

    static void Case(string root, string name, Func<string, string> before, Func<string, string> attack)
    {
        var folder = Path.Combine(root, name);
        Directory.CreateDirectory(folder);
        var path = Path.Combine(folder, "h.exe");
        Fill(path, (byte)'A');
        string original;
        using (var probe = CreateFileW(path, GENERIC_READ, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero)) original = Hash(probe);
        string setup = before == null ? "-" : before(path);
        using (var pin = Pin(path))
        {
            if (pin.IsInvalid)
            {
                Console.WriteLine("PROBE " + name + " setup=" + setup + " pin=refused-" + Error());
                return;
            }
            string pinnedId = Id(pin);
            string pinnedHash = Hash(pin);
            string start = pinnedHash == original ? "A" : "not-A";
            string result;
            try { result = attack(path); } catch (Exception e) { result = "threw-" + e.GetType().Name; }
            Console.WriteLine("PROBE " + name + " setup=" + setup + " pin=ok(" + start + ") attack=" + result + " " + Look(path, pin, pinnedId, pinnedHash));
        }
    }

    static IntPtr view;
    static IntPtr section;

    static string MapBefore(string path, bool keepSection)
    {
        using (var writer = CreateFileW(path, GENERIC_READ | GENERIC_WRITE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero))
        {
            if (writer.IsInvalid) return "open-" + Error();
            section = CreateFileMappingW(writer, IntPtr.Zero, PAGE_READWRITE, 0, 0, IntPtr.Zero);
            if (section == IntPtr.Zero) return "section-" + Error();
            view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, UIntPtr.Zero);
            if (view == IntPtr.Zero) return "view-" + Error();
        }
        if (!keepSection) { CloseHandle(section); section = IntPtr.Zero; }
        return keepSection ? "view+section-open,file-handle-closed" : "view-only,file-and-section-closed";
    }

    static string WriteView()
    {
        if (view == IntPtr.Zero) return "no-view";
        for (int i = 0; i < 4096; i++) Marshal.WriteByte(view, i, (byte)'B');
        bool flushed = FlushViewOfFile(view, UIntPtr.Zero);
        UnmapViewOfFile(view);
        view = IntPtr.Zero;
        if (section != IntPtr.Zero) { CloseHandle(section); section = IntPtr.Zero; }
        return "view-written,flush=" + flushed;
    }

    static string Swap(string path, Func<string, string> remove)
    {
        string removed = remove(path);
        try { Fill(path, (byte)'D'); return removed + ",new-at-path"; }
        catch (Exception e) { return removed + ",new-at-path-failed-" + e.GetType().Name; }
    }

    /// <summary>Runs every case under the folder. Input: the folder. Output: PROBE lines.</summary>
    public static void Run(string root)
    {
        Case(root, "2c1-map-before-pin-section-closed", p => MapBefore(p, false), p => WriteView());
        Case(root, "2c2-map-before-pin-section-open", p => MapBefore(p, true), p => WriteView());
        Case(root, "2c3-map-after-pin", null, p =>
        {
            using (var writer = CreateFileW(p, GENERIC_READ | GENERIC_WRITE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero))
            {
                if (writer.IsInvalid) return "open-" + Error();
                section = CreateFileMappingW(writer, IntPtr.Zero, PAGE_READWRITE, 0, 0, IntPtr.Zero);
                if (section == IntPtr.Zero) return "section-" + Error();
                view = MapViewOfFile(section, FILE_MAP_WRITE, 0, 0, UIntPtr.Zero);
            }
            return WriteView();
        });
        Case(root, "2a-write-open-after-pin", null, p =>
        {
            using (var writer = CreateFileW(p, GENERIC_WRITE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero))
                return writer.IsInvalid ? "open-" + Error() : Write(writer, (byte)'W');
        });
        Case(root, "2d1-nt-supersede-delete-only", null, p => NtOpen(p, DELETE, 0));
        Case(root, "2d2-nt-supersede-write", null, p => NtOpen(p, GENERIC_WRITE | DELETE, 0));
        Case(root, "2d3-nt-overwrite-write", null, p => NtOpen(p, GENERIC_WRITE, 4));
        Case(root, "2d4-nt-overwrite-attributes-delete", null, p => NtOpen(p, FILE_WRITE_ATTRIBUTES | DELETE, 4));
        Case(root, "2d5-nt-overwrite-if-delete", null, p => NtOpen(p, DELETE, 5));
        Case(root, "2d6-create-always-delete-only", null, p =>
        {
            using (var h = CreateFileW(p, DELETE, SHARE_ALL, IntPtr.Zero, CREATE_ALWAYS, 0, IntPtr.Zero))
                return h.IsInvalid ? "open-" + Error() : "ok";
        });
        Case(root, "2d7-truncate-existing-write", null, p =>
        {
            using (var h = CreateFileW(p, GENERIC_WRITE, SHARE_ALL, IntPtr.Zero, TRUNCATE_EXISTING, 0, IntPtr.Zero))
                return h.IsInvalid ? "open-" + Error() : "ok";
        });
        Case(root, "2d8-hard-link-then-write", null, p =>
        {
            var link = p + ".link";
            if (!CreateHardLinkW(link, p, IntPtr.Zero)) return "link-" + Error();
            using (var h = CreateFileW(link, GENERIC_WRITE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero))
                return "linked," + (h.IsInvalid ? "open-" + Error() : Write(h, (byte)'L'));
        });
        Case(root, "2d9-set-basic-info-times", null, p =>
        {
            using (var h = CreateFileW(p, FILE_WRITE_ATTRIBUTES, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero))
            {
                if (h.IsInvalid) return "open-" + Error();
                var info = new byte[40];
                BitConverter.GetBytes(DateTime.UtcNow.AddDays(-30).ToFileTimeUtc()).CopyTo(info, 16);
                BitConverter.GetBytes(DateTime.UtcNow.AddDays(-30).ToFileTimeUtc()).CopyTo(info, 24);
                return SetFileInformationByHandle(h, 0, info, 40) ? "write-and-change-time-set" : "set-" + Error();
            }
        });
        Case(root, "4a-rename-aside-then-new", null, p => Swap(p, q => MoveFileExW(q, q + ".old", 0) ? "renamed-aside" : "rename-" + Error()));
        Case(root, "4b1-movefileex-replace-over", null, p =>
        {
            var fresh = p + ".new";
            Fill(fresh, (byte)'E');
            return MoveFileExW(fresh, p, MOVEFILE_REPLACE_EXISTING) ? "replaced" : "move-" + Error();
        });
        Case(root, "4b2-posix-rename-replace-over", null, p =>
        {
            var fresh = p + ".new";
            Fill(fresh, (byte)'E');
            using (var h = CreateFileW(fresh, DELETE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero))
            {
                if (h.IsInvalid) return "open-" + Error();
                var buffer = RenameBuffer(p, 0x1 | 0x2);
                return SetFileInformationByHandle(h, 22, buffer, (uint)buffer.Length) ? "replaced" : "rename-" + Error();
            }
        });
        Case(root, "4d1-posix-delete-then-new", null, p => Swap(p, q =>
        {
            using (var h = CreateFileW(q, DELETE, SHARE_ALL, IntPtr.Zero, OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, IntPtr.Zero))
            {
                if (h.IsInvalid) return "open-" + Error();
                return SetFileInformationByHandle(h, 21, BitConverter.GetBytes((uint)(0x1 | 0x2)), 4) ? "posix-deleted" : "delete-" + Error();
            }
        }));
        Case(root, "4d2-plain-delete-then-new", null, p => Swap(p, q =>
        {
            try { File.Delete(q); return "deleted"; } catch (Exception e) { return "delete-" + e.GetType().Name; }
        }));
        Case(root, "4c-parent-rename-then-new", null, p =>
        {
            var folder = Path.GetDirectoryName(p);
            if (!MoveFileExW(folder, folder + "-moved", 0)) return "parent-rename-" + Error();
            Directory.CreateDirectory(folder);
            Fill(p, (byte)'F');
            return "parent-renamed,new-at-path";
        });
    }
}
