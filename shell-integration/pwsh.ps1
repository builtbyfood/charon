# Charon OSC 133 hook for PowerShell 5+/7. Wraps the existing prompt function.
# BEL-terminated OSC (see bash.sh).
if ($env:CHARON_HOOKED) { return }; $env:CHARON_HOOKED = 1
$global:__charon_orig_prompt = $function:prompt
function global:prompt {
  $code = if ($?) { 0 } else { if ($LASTEXITCODE) { $LASTEXITCODE } else { 1 } }
  $esc = [char]27; $bel = [char]7
  $cwd = "/" + ($PWD.ProviderPath -replace '\\', '/')
  "$esc]133;D;$code$bel$esc]7;file://$env:COMPUTERNAME$cwd$bel$esc]133;A$bel" + (& $global:__charon_orig_prompt) + "$esc]133;B$bel"
}
Set-PSReadLineKeyHandler -Key Enter -ScriptBlock {
  [Console]::Write("$([char]27)]133;C$([char]7)")
  [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
}
