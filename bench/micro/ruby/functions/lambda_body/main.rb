class Handler
  def call_it
    fn = ->(a) { transform(a) }
    fn.call(1)
  end

  def transform(a)
    a
  end
end
