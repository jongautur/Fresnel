using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

// Raw Native Wifi reader, independent of Fresnel, for checking lRssi,
// uLinkQuality and ullHostTimestamp and for timing WlanScan.
public static class WlanRaw
{
    [DllImport("wlanapi.dll")] static extern uint WlanOpenHandle(uint ver, IntPtr r, out uint neg, out IntPtr h);
    [DllImport("wlanapi.dll")] static extern uint WlanCloseHandle(IntPtr h, IntPtr r);
    [DllImport("wlanapi.dll")] static extern uint WlanEnumInterfaces(IntPtr h, IntPtr r, out IntPtr list);
    [DllImport("wlanapi.dll")] static extern uint WlanGetNetworkBssList(IntPtr h, ref Guid g, IntPtr ssid, int type, bool sec, IntPtr r, out IntPtr list);
    [DllImport("wlanapi.dll")] static extern uint WlanScan(IntPtr h, ref Guid g, IntPtr ssid, IntPtr ie, IntPtr r);
    [DllImport("wlanapi.dll")] static extern void WlanFreeMemory(IntPtr p);
    [DllImport("wlanapi.dll")] static extern uint WlanRegisterNotification(IntPtr h, uint src, bool ignoreDup, Callback cb, IntPtr ctx, IntPtr r, out uint prev);

    [StructLayout(LayoutKind.Sequential)]
    struct NotificationData { public uint Source; public uint Code; public Guid Interface; public uint Size; public IntPtr Data; }
    delegate void Callback(ref NotificationData d, IntPtr ctx);

    public class Bss
    {
        public string Ssid; public string Bssid; public int Rssi; public uint Quality;
        public uint FreqKhz; public ulong HostTs; public ulong TsfTs; public int Phy;
    }

    static IntPtr handle;
    static Callback keep;
    static AutoResetEvent done = new AutoResetEvent(false);
    static volatile uint lastCode;

    public static Guid Open()
    {
        uint neg;
        uint c = WlanOpenHandle(2, IntPtr.Zero, out neg, out handle);
        if (c != 0) throw new Exception("WlanOpenHandle " + c);
        IntPtr list;
        c = WlanEnumInterfaces(handle, IntPtr.Zero, out list);
        if (c != 0) throw new Exception("WlanEnumInterfaces " + c);
        int n = Marshal.ReadInt32(list, 0);
        if (n == 0) throw new Exception("no WLAN interfaces");
        byte[] g = new byte[16];
        Marshal.Copy(list + 8, g, 0, 16);
        WlanFreeMemory(list);
        keep = OnNotify;
        uint prev;
        c = WlanRegisterNotification(handle, 0x8 /* ACM */, true, keep, IntPtr.Zero, IntPtr.Zero, out prev);
        if (c != 0) throw new Exception("WlanRegisterNotification " + c);
        return new Guid(g);
    }

    static void OnNotify(ref NotificationData d, IntPtr ctx)
    {
        if (d.Code == 7 || d.Code == 8) { lastCode = d.Code; done.Set(); }
    }

    /// Returns elapsed ms, or -1 on timeout; code = 7 complete, 8 failed, other = WlanScan error.
    public static long Scan(Guid g, int timeoutMs, out uint code)
    {
        done.Reset();
        var sw = Stopwatch.StartNew();
        uint c = WlanScan(handle, ref g, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
        if (c != 0) { code = c; return sw.ElapsedMilliseconds; }
        bool ok = done.WaitOne(timeoutMs);
        code = ok ? lastCode : 0;
        return ok ? sw.ElapsedMilliseconds : -1;
    }

    public static List<Bss> List(Guid g)
    {
        IntPtr list;
        uint c = WlanGetNetworkBssList(handle, ref g, IntPtr.Zero, 3 /* any */, false, IntPtr.Zero, out list);
        if (c != 0) throw new Exception("WlanGetNetworkBssList " + c);
        var result = new List<Bss>();
        int n = Marshal.ReadInt32(list, 4);
        for (int i = 0; i < n; i++)
        {
            IntPtr e = list + 8 + i * 360;
            int len = Math.Min(Marshal.ReadInt32(e, 0), 32);
            byte[] ssid = new byte[len];
            Marshal.Copy(e + 4, ssid, 0, len);
            byte[] mac = new byte[6];
            Marshal.Copy(e + 40, mac, 0, 6);
            result.Add(new Bss
            {
                Ssid = Encoding.UTF8.GetString(ssid),
                Bssid = BitConverter.ToString(mac).Replace('-', ':'),
                Phy = Marshal.ReadInt32(e, 52),
                Rssi = Marshal.ReadInt32(e, 56),
                Quality = (uint)Marshal.ReadInt32(e, 60),
                TsfTs = (ulong)Marshal.ReadInt64(e, 72),
                HostTs = (ulong)Marshal.ReadInt64(e, 80),
                FreqKhz = (uint)Marshal.ReadInt32(e, 92),
            });
        }
        WlanFreeMemory(list);
        return result;
    }

    public static void Close() { if (handle != IntPtr.Zero) WlanCloseHandle(handle, IntPtr.Zero); }
}
