module example.com/shop

go 1.22

require (
	example.com/lib v0.0.0
	github.com/spf13/viper v1.19.0
)

replace example.com/lib => ../lib
