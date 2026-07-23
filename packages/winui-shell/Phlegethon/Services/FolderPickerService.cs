using System;
using System.Runtime.InteropServices;
using Microsoft.UI.Xaml;

namespace Phlegethon.Services;

/// <summary>
/// Win32 folder picker for unpackaged WinUI 3 desktop (reliable vs WinRT FolderPicker).
/// </summary>
internal static class FolderPickerService
{
    private const int ErrorCancelled = unchecked((int)0x800704C7);

    public static string? PickFolder(Window owner, string title)
    {
        var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(owner);
        var dialog = (IFileOpenDialog)new FileOpenDialog();
        dialog.SetOptions(
            FILEOPENDIALOGOPTIONS.FOS_PICKFOLDERS |
            FILEOPENDIALOGOPTIONS.FOS_FORCEFILESYSTEM |
            FILEOPENDIALOGOPTIONS.FOS_PATHMUSTEXIST);
        dialog.SetTitle(title);

        try
        {
            var hr = dialog.Show(hwnd);
            if (hr == ErrorCancelled)
            {
                return null;
            }

            if (hr < 0)
            {
                Marshal.ThrowExceptionForHR(hr);
            }

            dialog.GetResult(out var item);
            item.GetDisplayName(SIGDN.FILESYSPATH, out var path);
            return path;
        }
        catch (COMException ex) when (ex.HResult == ErrorCancelled)
        {
            return null;
        }
    }

    [ComImport]
    [Guid("42F85136-DB7E-439C-85F1-E4075D135FC8")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IFileOpenDialog
    {
        [PreserveSig]
        int Show(IntPtr parent);

        void SetFileTypes();
        void SetFileTypeIndex(uint iFileType);
        void GetFileTypeIndex(out uint piFileType);
        void Advise();
        void Unadvise();
        void SetOptions(FILEOPENDIALOGOPTIONS fos);
        void GetOptions(out FILEOPENDIALOGOPTIONS pfos);
        void SetDefaultFolder(IShellItem psi);
        void SetFolder(IShellItem psi);
        void GetFolder(out IShellItem ppsi);
        void GetCurrentSelection(out IShellItem ppsi);
        void SetFileName([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        void GetFileName([MarshalAs(UnmanagedType.LPWStr)] out string pszName);
        void SetTitle([MarshalAs(UnmanagedType.LPWStr)] string pszTitle);
        void SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string pszText);
        void SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string pszLabel);
        void GetResult(out IShellItem ppsi);
        void AddPlace(IShellItem psi, int alignment);
        void SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string pszDefaultExtension);
        void Close(int hr);
        void SetClientGuid();
        void ClearClientData();
        void SetFilter();
        void GetResults(out IShellItemArray ppenum);
        void GetSelectedItems(out IShellItemArray ppsai);
    }

    [ComImport]
    [Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        void BindToHandler();
        void GetParent();
        void GetDisplayName(SIGDN sigdnName, [MarshalAs(UnmanagedType.LPWStr)] out string ppszName);
        void GetAttributes();
        void Compare();
    }

    [ComImport]
    [Guid("DC1C5A9C-E88A-4dde-B5A1-9F464360F612")]
    private class FileOpenDialog
    {
    }

    [Flags]
    private enum FILEOPENDIALOGOPTIONS : uint
    {
        FOS_PICKFOLDERS = 0x00000020,
        FOS_FORCEFILESYSTEM = 0x00000040,
        FOS_PATHMUSTEXIST = 0x00000800,
    }

    private enum SIGDN : uint
    {
        FILESYSPATH = 0x80058000,
    }

    [ComImport]
    [Guid("7E6FA3DD-6043-4B89-9BB2-4407E52C7E6B")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItemArray
    {
    }
}
