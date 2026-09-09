# U-005 live semantic identity + live rd_* redirect-buffer read-back.
#
# Runs linuxjampded as the inferior (new engine instance, port 29101) and sets
# breakpoints at the addresses the proxy's table names.  Each breakpoint
# prints a marker + short backtrace, then continues.  The final breakpoint
# (SV_FlushRedirect, 0x8057b44) prints the live rd_*/rd_buffersize/rd_flush
# slots, then disables everything, detaches and quits.
#
# Drive it from outside: wait for the server, run getchallenge/getinfo/
# getstatus, then an rcon packet.  See exp/u005_semantic.drive.py.

set pagination off
set confirm off
set print pretty off

break *0x08056d64
commands
  silent
  printf "\n=== BP SV_ConnectionlessPacket @0x8056d64 ===\n"
  bt 3
  continue
end

break *0x0804b9a4
commands
  silent
  printf "\n=== BP SV_GetChallenge @0x804b9a4 ===\n"
  bt 3
  continue
end

break *0x08056784
commands
  silent
  printf "\n=== BP SVC_Info @0x8056784 ===\n"
  bt 4
  continue
end

break *0x08056574
commands
  silent
  printf "\n=== BP SVC_Status @0x8056574 ===\n"
  bt 3
  continue
end

break *0x08056b14
commands
  silent
  printf "\n=== BP SVC_RemoteCommand @0x8056b14 ===\n"
  bt 3
  continue
end

break *0x08057b44
commands
  silent
  printf "\n=== BP SV_FlushRedirect @0x8057b44 (rd_* read-back) ===\n"
  printf "rd_buffer     @0x081e90c0 = 0x%08x\n", *(unsigned int*)0x081e90c0
  printf "rd_buffersize @0x081e90c4 = 0x%08x\n", *(unsigned int*)0x081e90c4
  printf "rd_flush      @0x081e90c8 = 0x%08x\n", *(unsigned int*)0x081e90c8
  x/s *(char**)0x081e90c0
  bt 4
  disable 1 2 3 4 5 6
  detach
  quit
end

run +set dedicated 1 +set net_port 29101 \
    +set fs_cdpath /srv/jka/gamedata \
    +set sv_pure 0 \
    +set rconPassword testpw \
    +set sv_hostname u005-semantic \
    +map mp/duel1
