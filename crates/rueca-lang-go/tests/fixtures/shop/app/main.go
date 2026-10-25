// Command shop serves the shop API.
package main

import (
	"embed"
	j "encoding/json"
	"fmt"
	"os"

	"example.com/lib/money"
	"example.com/shop/internal/store"
	"example.com/shop/pkg/termx"
	"example.com/shop/server"
	. "example.com/shop/util"
	"example.com/tools/gen"
	"github.com/spf13/viper"
)

//go:embed static
var static embed.FS

// Version is the build version.
const Version = "1.0"

var defaultServer = server.NewServer(nil)

func main() {
	viper.SetConfigName("shop")
	port := viper.GetString("server.port")
	dsn := os.Getenv("DATABASE_URL")
	srv := server.NewServer(store.New())
	srv.Start()
	var s server.Server
	s.Handle(nil)
	defaultServer.Start()
	price, _ := money.Parse(dsn)
	fmt.Println(price.String(), Clamp(1, 2, 3), term.Width(), port)
	gen.Run()
	b, _ := j.Marshal(Version)
	_ = b
	_ = static
}
