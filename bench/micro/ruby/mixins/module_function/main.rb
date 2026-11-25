module Util
  module_function

  def slug(s)
    clean(s)
  end

  def clean(s)
    s
  end
end

def run
  Util.slug("a")
end
