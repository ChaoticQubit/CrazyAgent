$ErrorActionPreference = 'Stop'
$env:PSModulePath = Join-Path $PSHOME 'Modules'
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class ClipProbe {
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern IntPtr CreateWindowExW(uint ex, string cls, string name, uint style, int x, int y,
        int w, int h, IntPtr parent, IntPtr menu, IntPtr inst, IntPtr param);
    [DllImport("user32.dll")] static extern bool DestroyWindow(IntPtr h);
    [DllImport("user32.dll")] static extern bool OpenClipboard(IntPtr h);
    [DllImport("user32.dll")] static extern bool CloseClipboard();
    [DllImport("user32.dll")] static extern bool EmptyClipboard();
    [DllImport("user32.dll")] static extern IntPtr SetClipboardData(uint f, IntPtr h);
    [DllImport("user32.dll")] static extern IntPtr GetClipboardData(uint f);
    [DllImport("user32.dll")] static extern uint EnumClipboardFormats(uint f);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalAlloc(uint flags, UIntPtr bytes);
    [DllImport("kernel32.dll")] static extern IntPtr GlobalLock(IntPtr h);
    [DllImport("kernel32.dll")] static extern bool GlobalUnlock(IntPtr h);
    [DllImport("kernel32.dll")] static extern UIntPtr GlobalSize(IntPtr h);

    static bool Open(IntPtr w) {
        for (int i = 0; i < 50; i++) {
            if (OpenClipboard(w)) return true;
            Thread.Sleep(20);
        }
        return false;
    }

    static string Units(IntPtr block, int most) {
        ulong size = GlobalSize(block).ToUInt64();
        IntPtr p = GlobalLock(block);
        if (p == IntPtr.Zero) return "size " + size + " bytes, lock failed";
        var hex = new StringBuilder();
        int n = (int)Math.Min(size / 2, (ulong)most);
        for (int i = 0; i < n; i++) {
            hex.Append(((ushort)Marshal.ReadInt16(p, i * 2)).ToString("X4")).Append(' ');
        }
        GlobalUnlock(block);
        return "size " + size + " bytes, units " + hex.ToString().Trim();
    }

    public static string Run(string label, int[] units) {
        IntPtr w = CreateWindowExW(0, "STATIC", null, 0, 0, 0, 0, 0, new IntPtr(-3),
            IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (w == IntPtr.Zero) return label + ": no window";
        try {
            if (!Open(w)) return label + ": open for write failed";
            EmptyClipboard();
            IntPtr h = GlobalAlloc(2, new UIntPtr((uint)(units.Length * 2)));
            IntPtr p = GlobalLock(h);
            for (int i = 0; i < units.Length; i++) {
                Marshal.WriteInt16(p, i * 2, unchecked((short)units[i]));
            }
            GlobalUnlock(h);
            string before = Units(h, 12);
            bool set = SetClipboardData(13, h) != IntPtr.Zero;
            CloseClipboard();
            if (!Open(w)) return label + ": open for read failed";
            var formats = new List<string>();
            for (uint f = EnumClipboardFormats(0); f != 0; f = EnumClipboardFormats(f)) {
                formats.Add(f.ToString());
            }
            IntPtr r = GetClipboardData(13);
            string after = r == IntPtr.Zero ? "no CF_UNICODETEXT" : Units(r, 12);
            CloseClipboard();
            return label + ": wrote " + units.Length + " units (" + before + "), set " + set
                + ", formats [" + string.Join(",", formats) + "], read " + after;
        } finally {
            DestroyWindow(w);
        }
    }
}
'@
$cases = [ordered]@{
    'abc, no NUL' = @(0x61, 0x62, 0x63)
    'abc with NUL' = @(0x61, 0x62, 0x63, 0)
    'one unit, no NUL' = @(0x61)
    'lone high surrogate' = @(0xD800, 0x61, 0)
    'two NULs' = @(0x61, 0, 0x62, 0)
}
foreach ($label in $cases.Keys) {
    'probe: ' + [ClipProbe]::Run($label, [int[]]$cases[$label])
}
