import matplotlib.pyplot as plt
import sys

kind = "thrift"


# 0-name, 1-concurrency, 2-size, 3-qps, 6-p99, 7-p999
def parse_data(file):
    import csv
    csv_reader = csv.reader(open(file))
    lines = []
    for line in csv_reader:
        lines.append(line)
    x_label, x_ticks = parse_x(lines=lines)
    print(x_label, x_ticks)

    y_qps = parse_y(lines=lines, idx=3)
    print(y_qps)
    plot_data(title="QPS (higher is better)", xlabel=x_label, ylabel="qps", x_ticks=x_ticks, ys=y_qps)

    y_p99 = parse_y(lines=lines, idx=4, times=1000)
    print(y_p99)
    plot_data(title="TP99 (lower is better)", xlabel=x_label, ylabel="latency(us)", x_ticks=x_ticks, ys=y_p99)

    y_p999 = parse_y(lines=lines, idx=5, times=1000)
    print(y_p999)
    plot_data(title="TP999 (lower is better)", xlabel=x_label, ylabel="latency(us)", x_ticks=x_ticks, ys=y_p999)


# 并发相同比 size; size 相同比并发
def parse_x(lines):
    l = len(lines)
    idx = 1
    x_label = "concurrency"
    if lines[0][1] == lines[l - 1][1]:
        idx = 2
        x_label = "echo size(Byte)"
    x_list = []
    x_key = lines[0][0]
    for line in lines:
        if line[0] == x_key:
            x_list.append(int(line[idx]))
    return x_label, x_list


def parse_y(lines, idx, times=1):
    y_dict = {}
    for line in lines:
        name = line[0]
        y_line = y_dict.get(name, [])
        n = float(line[idx]) * times
        y_line.append(int(n))
        # y_line.append(int(line[idx]))
        y_dict[name] = y_line
    return y_dict


# TODO
color_dict = {
    "[thrift]": "royalblue",
}


# ys={"$net":[]number}
def plot_data(title, xlabel, ylabel, x_ticks, ys):
    plt.figure(figsize=(8, 5))
    # bmh、ggplot、dark_background、fivethirtyeight 和 grayscale
    plt.style.use('grayscale')
    plt.title(title)

    plt.xlabel(xlabel)
    plt.ylabel(ylabel)

    # x 轴示数
    plt.xticks(range(len(x_ticks)), x_ticks)

    for k, v in ys.items():
        color = color_dict.get(k)
        if color != "":
            plt.plot(v, label=k, linewidth=2, color=color)
        else:
            plt.plot(v, label=k, linewidth=2)

    # y 轴从 0 开始
    bottom, top = plt.ylim()
    plt.ylim(bottom=0, top=1.2 * top)

    plt.legend(prop={'size': 12})
    plt.savefig("{0}_{1}.png".format(kind, title.split(" ")[0].lower()))
    # plt.show()


if __name__ == '__main__':
    if len(sys.argv) > 1:
        kind = sys.argv[1]
    parse_data(file="{0}.csv".format(kind))
