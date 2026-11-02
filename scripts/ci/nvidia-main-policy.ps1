function Assert-NvidiaMainJob {
    param([string]$Repository, [string]$Ref, [string]$EventName, [string]$Revision, [string]$MainRevision)
    if ($Repository -cne 'azerothl/Akasha' -or $Ref -cne 'refs/heads/main' -or
        $EventName -notin @('push', 'workflow_dispatch') -or $Revision -notmatch '^[0-9a-f]{40}$' -or
        $Revision -cne $MainRevision) {
        throw 'NVIDIA runner accepts only the current Akasha main revision, from push or manual dispatch.'
    }
}
