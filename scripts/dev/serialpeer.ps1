# Reference serial peer for hardware tests (uses .NET SerialPort, i.e. overlapped I/O).
#
#   echo   – send back everything received, for -Seconds
#   duplex – on the first received byte, start sending -Bytes of the test pattern
#            while collecting -Bytes and checking them against the pattern (seed 2)
#   demo   – print ANSI colours and Korean text every second, echo input (UI checks)
#
# The pattern matches `pattern()` in src-tauri/src/transport/serial_hw_tests.rs.
param(
  [Parameter(Mandatory)][string]$Port,
  [int]$Baud = 115200,
  [ValidateSet('echo', 'duplex', 'demo')][string]$Mode = 'echo',
  [int]$Bytes = 65536,
  [int]$Seconds = 30
)
$ErrorActionPreference = 'Stop'

function Get-Pattern([int]$len, [int]$seed) {
  $b = New-Object byte[] $len
  for ($i = 0; $i -lt $len; $i++) { $b[$i] = [byte]((($i * 7) + (($i -shr 8) * 13) + $seed) -band 0xFF) }
  return , $b
}

$sp = New-Object System.IO.Ports.SerialPort $Port, $Baud, 'None', 8, 'One'
$sp.ReadBufferSize = 4MB
$sp.WriteBufferSize = 4MB
$sp.DtrEnable = $true
$sp.RtsEnable = $true
$sp.Open()
$sp.DiscardInBuffer()
"READY $Port $Mode"
$sw = [Diagnostics.Stopwatch]::StartNew()
$buf = New-Object byte[] 65536
try {
  switch ($Mode) {
    'echo' {
      $n = 0
      while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        $avail = $sp.BytesToRead
        if ($avail) {
          $k = $sp.Read($buf, 0, [Math]::Min($avail, $buf.Length))
          $sp.BaseStream.Write($buf, 0, $k)
          $n += $k
        } else { Start-Sleep -Milliseconds 1 }
      }
      "ECHOED $n"
    }
    'duplex' {
      $out = Get-Pattern $Bytes 1
      $want = Get-Pattern $Bytes 2
      $got = New-Object byte[] $Bytes
      $n = 0
      $send = $null
      while ($n -lt $Bytes -and $sw.Elapsed.TotalSeconds -lt $Seconds) {
        $avail = $sp.BytesToRead
        if ($avail) {
          if (-not $send) { $send = $sp.BaseStream.WriteAsync($out, 0, $Bytes) }
          $n += $sp.Read($got, $n, [Math]::Min($avail, $Bytes - $n))
        } else { Start-Sleep -Milliseconds 1 }
      }
      if ($send) { [void]$send.Wait(15000) }
      $bad = 0; $first = -1
      for ($i = 0; $i -lt $n; $i++) { if ($got[$i] -ne $want[$i]) { $bad++; if ($first -lt 0) { $first = $i } } }
      "PEER_RECEIVED $n/$Bytes mismatched=$bad first_bad=$first sent=$([int]($send -and $send.IsCompleted))"
    }
    'demo' {
      $e = [char]27
      $i = 0
      while ($sw.Elapsed.TotalSeconds -lt $Seconds) {
        $i++
        $line = "$e[32m[$i]$e[0m serial demo $e[1;33mCOM$e[0m $e[36m한글 출력 테스트$e[0m $e[41;97m ERR $e[0m`r`n"
        $msg = [Text.Encoding]::UTF8.GetBytes($line)
        $sp.BaseStream.Write($msg, 0, $msg.Length)
        $until = $sw.ElapsedMilliseconds + 1000
        while ($sw.ElapsedMilliseconds -lt $until) {
          $avail = $sp.BytesToRead
          if ($avail) {
            $k = $sp.Read($buf, 0, [Math]::Min($avail, $buf.Length))
            $sp.BaseStream.Write($buf, 0, $k)
          } else { Start-Sleep -Milliseconds 5 }
        }
      }
      "DEMO_DONE"
    }
  }
} finally {
  $sp.Close()
}
