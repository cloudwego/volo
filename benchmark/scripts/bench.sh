#!/bin/bash
set -e

CURDIR=$(cd $(dirname $0); pwd)

echo "Checking whether the environment meets the requirements ..."
source $CURDIR/env.sh
echo "Check finished."

ports=(8001)

echo "Building thrift services by exec build_thrift.sh ..."
source $CURDIR/build.sh
echo "Build finished."

if [[ $1 == "client" ]]; then
    t=$cmd_server
    cmd_server=$cmd_client
    cmd_client=$t
fi

# benchmark
for b in ${body[@]}; do
  for c in ${concurrent[@]}; do
    for q in ${qps[@]}; do
      addr="127.0.0.1:${ports[0]}"
      kill_pid_listening_on_port ${ports[0]}
      # server start
      echo "Starting server, if failed please check [output/log/nohup.log] for detail"
      nohup $cmd_server $output_dir/bin/bench-server >> $output_dir/log/nohup.log 2>&1 &
      sleep 1
      echo "Server running with [$cmd_server]"

      # run client
      echo "Client running with [$cmd_client]"
      $cmd_client $output_dir/bin/bench-client -a="$addr" -b=$b -c=$c -q=$q -n=$n -s=$sleep | $tee_cmd

      # stop server
      kill_pid_listening_on_port ${ports[0]}
    done
  done
done

finish_cmd