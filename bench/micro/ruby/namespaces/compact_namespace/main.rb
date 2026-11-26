module App
  class Error
    def self.wrap
      1
    end
  end
end

class App::Runner
  def go
    App::Error.wrap
  end
end
