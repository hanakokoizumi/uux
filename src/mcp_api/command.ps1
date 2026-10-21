& {
    $uu_output_prefix=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__OUTPUT_PREFIX__'));
    $uu_done_prefix=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('__DONE_PREFIX__'));
    function UuEmit([string]$value) { [Console]::WriteLine($uu_output_prefix+[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($value))+':UU_OUT_END') };
    function UuChunk([string]$value,[ref]$pending) {
        $value=$pending.Value+$value; $pending.Value='';
        if ($value.Length -gt 0 -and [char]::IsHighSurrogate($value[$value.Length-1])) { $pending.Value=$value.Substring($value.Length-1); $value=$value.Substring(0,$value.Length-1) };
        if ($value.Length -gt 0) { UuEmit $value };
    };
    $uu_opending=''; $uu_epending='';
    $uu_process=[Diagnostics.Process]::new();
    $uu_process.StartInfo.FileName='powershell.exe';
    $uu_process.StartInfo.Arguments='-NoLogo -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand __SCRIPT__';
    $uu_process.StartInfo.UseShellExecute=$false;
    $uu_process.StartInfo.CreateNoWindow=$true;
    $uu_process.StartInfo.RedirectStandardOutput=$true;
    $uu_process.StartInfo.RedirectStandardError=$true;
    $uu_process.StartInfo.StandardOutputEncoding=[Text.Encoding]::UTF8;
    $uu_process.StartInfo.StandardErrorEncoding=[Text.Encoding]::UTF8;
    $uu_code=1;
    try {
        [void]$uu_process.Start();
        $uu_ob=[char[]]::new(1024); $uu_eb=[char[]]::new(1024);
        $uu_ot=$uu_process.StandardOutput.ReadAsync($uu_ob,0,$uu_ob.Length);
        $uu_et=$uu_process.StandardError.ReadAsync($uu_eb,0,$uu_eb.Length);
        while ($null -ne $uu_ot -or $null -ne $uu_et) {
            if ($null -ne $uu_ot -and $uu_ot.IsCompleted) {
                $uu_n=$uu_ot.GetAwaiter().GetResult();
                if ($uu_n -gt 0) { UuChunk ([string]::new($uu_ob,0,$uu_n)) ([ref]$uu_opending); $uu_ot=$uu_process.StandardOutput.ReadAsync($uu_ob,0,$uu_ob.Length) } else { $uu_ot=$null };
            };
            if ($null -ne $uu_et -and $uu_et.IsCompleted) {
                $uu_n=$uu_et.GetAwaiter().GetResult();
                if ($uu_n -gt 0) { UuChunk ([string]::new($uu_eb,0,$uu_n)) ([ref]$uu_epending); $uu_et=$uu_process.StandardError.ReadAsync($uu_eb,0,$uu_eb.Length) } else { $uu_et=$null };
            };
            if ($null -ne $uu_ot -or $null -ne $uu_et) { Start-Sleep -Milliseconds 15 };
        };
        $uu_process.WaitForExit(); $uu_code=$uu_process.ExitCode;
    } catch { UuEmit ($_ | Out-String) } finally { $uu_process.Dispose() };
    [Console]::WriteLine($uu_done_prefix+$uu_code.ToString()+':UU_END');
};
