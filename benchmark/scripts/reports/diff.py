import csv

import sys

'''CSV Format Example
Kind,Concurrency,Data Size,TPS,P99,P999,Server_CPU,Client_CPU
[GRPC],100,1024,101152.29,3.36,5.30,188.04,423.07
'''


class bcolors:
    HEADER = '\033[95m'
    OKBLUE = '\033[94m'
    OKCYAN = '\033[96m'
    OKGREEN = '\033[92m'
    WARNING = '\033[93m'
    FAIL = '\033[91m'
    ENDC = '\033[0m'
    BOLD = '\033[1m'
    UNDERLINE = '\033[4m'


def diff(from_csv, to_csv):
    from_reader = list(csv.reader(open(from_csv)))
    to_reader = csv.reader(open(to_csv))
    title = ['Kind', 'Concurrency', 'Data Size', 'QPS', 'P99', 'P999', 'Client CPU', 'Server CPU']
    results = []

    for line_num, line in enumerate(to_reader):
        result = []
        result.append(line[0])  # kind
        result.append(line[1])  # concurrency
        result.append(line[2])  # data size

        result.append(diff_cell(from_reader[line_num][3], line[3]))  # tps
        result.append(diff_cell(from_reader[line_num][4], line[4]))  # p99
        result.append(diff_cell(from_reader[line_num][5], line[5]))  # p999
        result.append(diff_cell(from_reader[line_num][6], line[6]))  # Server CPU
        result.append(diff_cell(from_reader[line_num][7], line[7]))  # Client CPU

        results.append(result)

    results.sort(key=lambda result: result[0])
    results.insert(0, title)
    print_csv(results)


def diff_cell(old, now):
    old, now = float(old), float(now)
    percent = (now - old) / old * 100
    flag = '+' if percent >= 0 else ''
    return '{}{}({}{:.1f}%){}'.format(now, bcolors.WARNING, flag, percent, bcolors.ENDC)


def print_csv(results):
    cell_size = 15
    for line in results:
        result = []
        for cell in line:
            padding = cell_size - len(cell)
            if padding <= 0:
                padding = 5
            cell += ' ' * padding
            result.append(cell)
        print(''.join(result))


def main():
    if len(sys.argv) < 3:
        print('''Usage:
diff.py {baseline.csv} {current.csv} 
''')
        return
    from_csv = sys.argv[1]
    to_csv = sys.argv[2]
    diff(from_csv, to_csv)


if __name__ == '__main__':
    main()
