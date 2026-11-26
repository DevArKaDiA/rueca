require_relative "app/parser"

class App
  def run
    Parser.new.parse
  end
end
