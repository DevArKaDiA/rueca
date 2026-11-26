import Lib

func show() -> String {
    return format(1) + Formatter().run(2)
}

print(show())
