#!/bin/bash
# vm.sh CMD ARGS : runs INSIDE a bench VM (ferrofire bench/range/run_range.py ships it in /root/cb/cb.tgz and calls it through minimega cc).
#   setup                       hosts file + multicast route (the flat ICS VLAN has no default route, so IP_ADD_MEMBERSHIP / RADIO need 224/4)
#   stop                        kill every bench process
#   prov STACK R                provider: ferrofire run --stamp-writes | stock pybennu PyPower (bench/stock_provider.py)
#   fd STACK R N                field device of rtu-N: ferrofire field-device | bennu-field-deviced
#   fdlog                       the field device's output (stock: its daemon stderr log, and the error/warning lines of its stdout log, which is otherwise a status dump per cycle)
#   wait PORT SECS              wait for a listening TCP port
#   drv STACK R REP BASE CELLS  SCADA driver ffcontrol in the background; output out/BASE.*, out/BASE.done holds its exit code
cd /root/cb || exit 1
G=/root/cb/grid; O=/root/cb/out; mkdir -p $O
bg() { setsid nohup "$@" </dev/null & }
case $1 in
setup)
  IF=$(ip -o -4 addr show | awk '/ 10\.1\.1\./{print $2; exit}')
  ip route replace 224.0.0.0/4 dev "$IF"
  sed -i '/ provider$/d;/ rtu-[0-9]*$/d' /etc/hosts
  { echo "10.1.1.10 provider"; for n in 1 2 3 4 5; do echo "10.1.1.$((100+n)) rtu-$n"; done; } >> /etc/hosts
  echo "ok $(hostname) $IF" ;;
stop)
  pkill -f '/root/cb/ferrofire'; pkill -f stock_provider.py; pkill -f ffcontrol; pkill -f bennu-field-dev; sleep 1
  pkill -9 -f '/root/cb/ferrofire'; pkill -9 -f stock_provider.py; pkill -9 -f bennu-field-dev; pkill -9 -f ffcontrol; rm -f /var/run/default-bennu-field-deviced.pid; sleep 1; echo stopped ;;
prov)
  rm -f $O/prov.log
  if [ "$2" = ferrofire ]; then bg /root/cb/ferrofire run $G/R$3/model.yaml --stamp-writes >$O/prov.log 2>&1
  else cp $G/case.py /etc/sceptre/case_control.py; bg python3 /root/cb/stock_provider.py start -c $G/R$3/config.ini >$O/prov.log 2>&1; fi
  echo started ;;
fd)  # stock: a relative path (the loader prepends ./ and breaks an absolute one)
  rm -f $O/fd.log
  if [ "$2" = ferrofire ]; then bg /root/cb/ferrofire field-device --file $G/R$3/ferrofire/rtu-$4.xml >$O/fd.log 2>&1
  else rm -f /etc/sceptre/log/default-bennu-field-deviced-std*.log; bennu-field-deviced --command start --file grid/R$3/bennu/rtu-$4.xml >$O/fd.log 2>&1 </dev/null; fi
  echo started ;;
fdlog) cat $O/fd.log /etc/sceptre/log/default-bennu-field-deviced-stderr.log 2>/dev/null; grep -i -E 'error|warn|fail|timeout|exception' /etc/sceptre/log/default-bennu-field-deviced-stdout.log 2>/dev/null ;;
wait)
  for _ in $(seq 1 "$3"); do ss -ltn | grep -q ":$2 " && { echo up; exit 0; }; sleep 1; done; echo TIMEOUT ;;
drv)
  S=$2; R=$3; K=$4; B=$5; C=$6
  if [ "$S" = stock ]; then X="--read-per 1 --legacy --status-only --fresh"; ST="udp://*;239.0.0.1:40000"; else X="--read-per 100"; ST="udp://*;239.0.1.{n}:40000"; fi
  rm -f $O/$B.*
  bg bash -c "/root/cb/ffcontrol --scada $G/R$R/scada.json --provider tcp://provider:5555 --stream '$ST' --cells $C --stack $S $X --rep $K --out $O/$B >$O/$B.driver.log 2>&1; echo \$? >$O/$B.done"
  echo started ;;
esac
