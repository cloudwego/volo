#!/bin/bash
set -e

# benchmark params
n=20000000
body=(1024)
concurrent=(100 200 400 600 800 1000)
qps=(0)
sleep=0

CURDIR=$(cd $(dirname $0); pwd)

source $CURDIR/util.sh && check_supported_env

if ! [ -x "$(command -v taskset)" ]; then
  echo "Error: taskset is not installed." >&2
  exit 1
fi

# cpu binding
nprocs=$(getconf _NPROCESSORS_ONLN)
if [ $nprocs -lt 4 ]; then
  echo "Error: your environment should have at least 4 processors"
  exit 1
elif [ $nprocs -gt 20 ]; then
  nprocs=20
fi
scpu=$((nprocs > 16 ? 4 : nprocs / 4)) # max is 4 cpus
ccpu=$((nprocs-scpu))
scpu_cmd="taskset -c 0-$((scpu-1))"
ccpu_cmd="taskset -c ${scpu}-$((ccpu-1))"
if [ -x "$(command -v numactl)" ]; then
  # use numa affinity
  node0=$(numactl -H | grep "node 0" | head -n 1 | cut -f "4-$((3+scpu))" -d ' ' --output-delimiter ',')
  node1=$(numactl -H | grep "node 1" | head -n 1 | cut -f "4-$((3+ccpu))" -d ' ' --output-delimiter ',')
  scpu_cmd="numactl -C ${node0} -m 0"
  ccpu_cmd="numactl -C ${node1} -m 1"
fi

USER=$(whoami)
REPORT=${REPORT_PREFIX}${REPORT:-"$(date +%F-%H-%M)"}

nice_cmd=''
tee_cmd="tee -a $CURDIR/../output/${REPORT}.log"
function finish_cmd() {
  # to csv report
  $CURDIR/reports/to_csv.sh $CURDIR/../output/"$REPORT.log" > $CURDIR/../output/"$REPORT".csv
  echo "output reports: $CURDIR/../output/$REPORT.log, $CURDIR/../output/$REPORT.csv"
  cat $CURDIR/../output/"$REPORT.csv"
}
if [ "$USER" == "root" ]; then
    nice_cmd='nice -n -20'
fi
cmd_server="${nice_cmd} ${scpu_cmd}"
cmd_client="${nice_cmd} ${ccpu_cmd}"

# set dirs
output_dir=$CURDIR/../output

function kill_pid_listening_on_port() {
    port=$1
    if [ -z "$port" ]; then
        echo "invalid port"
        exit 1
    fi
    pids=`lsof -i ":$port" | grep LISTEN | awk '{print $2}' | uniq`
    for p in $pids; do
        echo killing $p...
        kill $p
    done
    sleep 1
}
