package main

type Job struct {
	steps int
}

func (j *Job) Run() int {
	j.prepare()
	return j.execute()
}

func (j *Job) prepare() {
	j.steps++
}

func (j *Job) execute() int {
	return j.steps
}

func main() {
	j := &Job{}
	_ = j.Run()
}
