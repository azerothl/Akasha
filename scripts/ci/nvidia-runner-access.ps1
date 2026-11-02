function Set-NvidiaRunnerAccess {
    param([Parameter(Mandatory)][string]$Root)
    $directory = [IO.DirectoryInfo]::new([IO.Path]::GetFullPath($Root))
    # Read/write only the discretionary ACL. Reapplying owner/audit metadata
    # through Set-Acl can require privileges absent in a normal user session.
    $acl = [IO.FileSystemAclExtensions]::GetAccessControl($directory, [Security.AccessControl.AccessControlSections]::Access)
    $acl.SetAccessRuleProtection($true, $false)
    foreach ($rule in @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))) { $acl.RemoveAccessRuleAll($rule) }
    foreach ($sid in @([Security.Principal.WindowsIdentity]::GetCurrent().User, [Security.Principal.SecurityIdentifier]::new('S-1-5-18'), [Security.Principal.SecurityIdentifier]::new('S-1-5-32-544'))) {
        $acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow'))
    }
    [IO.FileSystemAclExtensions]::SetAccessControl($directory, $acl)
}
