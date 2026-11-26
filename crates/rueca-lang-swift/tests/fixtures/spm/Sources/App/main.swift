import Core

struct Runner {
    let canvas: Canvas

    func run() {
        let p = Point(polar: 1, angle: 0)
        canvas.add(p)
        canvas.add(Point.origin().scaled(by: 2))
        makeCanvas().add(p)
        print(canvas.total())
        report("done")
    }

    func report(_ s: String) {
        print(s.shouted())
    }
}

Runner(canvas: makeCanvas()).run()
