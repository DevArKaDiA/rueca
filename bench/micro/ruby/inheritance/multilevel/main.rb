class A
  def ping
    1
  end
end

class B < A
end

class C < B
  def go
    ping
  end
end
